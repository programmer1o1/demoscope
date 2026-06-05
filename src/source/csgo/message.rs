// CS:GO net-message framing + the (type → name) map for protocol-13xxx demos.
//
// The id values are the `NET_Messages` / `SVC_Messages` enums from CS:GO's
// netmessages.proto (stable across the protocol-13xxx era). We only name the
// ones demoscope cares about; everything else is still framed correctly and
// reported as `Other`, so an unrecognised id never desyncs the walk.

// `protobuf` is a sibling of `source_demo` under the crate's CLI root; the
// relative path resolves in both the binary build (root = main.rs) and the lib
// build (root = lib.rs, main.rs included as `mod cli`).
use super::super::super::protobuf::Reader;

/// The CS:GO net/svc message ids we act on. Numeric values match the engine's
/// `NET_Messages` (0–7) and `SVC_Messages` (≥4) enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MsgKind {
    NetNop = 0,
    NetDisconnect = 1,
    NetTick = 4,
    NetStringCmd = 5,
    NetSignonState = 7,
    SvcServerInfo = 8,
    SvcSendTable = 9,
    SvcClassInfo = 10,
    SvcCreateStringTable = 12,
    SvcUpdateStringTable = 13,
    SvcPacketEntities = 26,
    SvcGameEvent = 25,
    SvcGameEventList = 30,
    /// Any framed-but-unrecognised id; the numeric type is preserved.
    Other(u32),
}

impl MsgKind {
    pub fn from_id(id: u32) -> MsgKind {
        match id {
            0 => MsgKind::NetNop,
            1 => MsgKind::NetDisconnect,
            4 => MsgKind::NetTick,
            5 => MsgKind::NetStringCmd,
            7 => MsgKind::NetSignonState,
            8 => MsgKind::SvcServerInfo,
            9 => MsgKind::SvcSendTable,
            10 => MsgKind::SvcClassInfo,
            12 => MsgKind::SvcCreateStringTable,
            13 => MsgKind::SvcUpdateStringTable,
            25 => MsgKind::SvcGameEvent,
            26 => MsgKind::SvcPacketEntities,
            30 => MsgKind::SvcGameEventList,
            other => MsgKind::Other(other),
        }
    }

    /// Human label for logs/diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            MsgKind::NetNop => "net_NOP",
            MsgKind::NetDisconnect => "net_Disconnect",
            MsgKind::NetTick => "net_Tick",
            MsgKind::NetStringCmd => "net_StringCmd",
            MsgKind::NetSignonState => "net_SignonState",
            MsgKind::SvcServerInfo => "svc_ServerInfo",
            MsgKind::SvcSendTable => "svc_SendTable",
            MsgKind::SvcClassInfo => "svc_ClassInfo",
            MsgKind::SvcCreateStringTable => "svc_CreateStringTable",
            MsgKind::SvcUpdateStringTable => "svc_UpdateStringTable",
            MsgKind::SvcGameEvent => "svc_GameEvent",
            MsgKind::SvcPacketEntities => "svc_PacketEntities",
            MsgKind::SvcGameEventList => "svc_GameEventList",
            MsgKind::Other(_) => "other",
        }
    }
}

/// One framed message: its kind plus the raw protobuf body (not yet decoded).
#[derive(Debug, Clone, Copy)]
pub struct NetMessage<'a> {
    pub id: u32,
    pub kind: MsgKind,
    pub body: &'a [u8],
}

/// Frame a CS:GO DEM_PACKET payload into its constituent protobuf messages.
///
/// Layout: `varint(type) varint(size) <size bytes>` repeated to end of buffer.
/// A truncated or malformed prefix stops the walk cleanly — we return what we
/// framed so far rather than erroring the whole demo (matching the project's
/// "skip the bad blob, keep going" posture).
pub fn scan_payload(payload: &[u8]) -> Vec<NetMessage<'_>> {
    let mut reader = Reader::new(payload);
    let mut out = Vec::new();
    loop {
        if reader.is_empty() {
            break;
        }
        let id = match reader.read_varint() {
            Ok(v) => v as u32,
            Err(_) => break,
        };
        let size = match reader.read_varint() {
            Ok(v) => v as usize,
            Err(_) => break,
        };
        let body = match reader.read_bytes(size) {
            Ok(b) => b,
            Err(_) => break,
        };
        out.push(NetMessage {
            id,
            kind: MsgKind::from_id(id),
            body,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a `varint(type) varint(size) body` frame. All test ids/sizes are
    // < 128 so they encode as a single byte.
    fn frame(id: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![id, body.len() as u8];
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn frames_sequential_messages() {
        let mut buf = frame(4, &[0x08, 0x2a]); // net_Tick, 2-byte body
        buf.extend(frame(26, &[0x10, 0x01, 0x02])); // svc_PacketEntities, 3-byte body
        let msgs = scan_payload(&buf);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].kind, MsgKind::NetTick);
        assert_eq!(msgs[0].body, &[0x08, 0x2a]);
        assert_eq!(msgs[1].kind, MsgKind::SvcPacketEntities);
        assert_eq!(msgs[1].body.len(), 3);
    }

    #[test]
    fn unknown_id_preserved_as_other() {
        let buf = frame(99, &[0xff]);
        let msgs = scan_payload(&buf);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].kind, MsgKind::Other(99));
        assert_eq!(msgs[0].id, 99);
    }

    #[test]
    fn truncated_tail_stops_cleanly() {
        // Valid frame, then a size that overruns the buffer.
        let mut buf = frame(4, &[0x01]);
        buf.extend_from_slice(&[9, 200]); // claims 200-byte body, none present
        let msgs = scan_payload(&buf);
        assert_eq!(msgs.len(), 1); // only the good frame survives
        assert_eq!(msgs[0].kind, MsgKind::NetTick);
    }

    // End-to-end framing check against a real CS:GO demo. Self-skips unless
    // CSGO_DEMO points at a file (the binary isn't in the repo), so CI stays
    // green. Run locally with:
    //   CSGO_DEMO="DEMOS TESTING/monasterydemo.dem" cargo test --lib csgo_real -- --nocapture
    #[test]
    fn csgo_real_demo_framing() {
        use super::super::super::super::protobuf::Reader;
        let path = match std::env::var("CSGO_DEMO") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("[skip] set CSGO_DEMO to run against a real demo");
                return;
            }
        };
        let data = std::fs::read(&path).expect("read CSGO_DEMO");
        assert!(&data[0..8] == b"HL2DEMO\0", "not an HL2DEMO container");

        // proto-4 CS:GO container: 6-byte packet header, 2-slot democmdinfo.
        const HEADER: usize = 1072;
        let democmdinfo = 76 * 2;
        let pkt_hdr = 6; // cmd(1) + tick(4) + playerslot(1)
        let le_i32 = |o: usize| i32::from_le_bytes(data[o..o + 4].try_into().unwrap());

        let mut offset = HEADER;
        let mut hist: std::collections::HashMap<u32, (usize, usize)> = std::collections::HashMap::new();
        let mut total_msgs = 0usize;
        let mut bad_bodies = 0usize;
        let mut packets = 0usize;

        while offset + pkt_hdr <= data.len() {
            let raw_cmd = data[offset];
            offset += pkt_hdr;
            match raw_cmd {
                7 => break,                       // DEM_STOP
                3 => {}                           // DEM_SYNCTICK, no payload
                1 | 2 => {                        // DEM_SIGNON | DEM_PACKET
                    if offset + democmdinfo + 12 > data.len() { break; }
                    let length = le_i32(offset + democmdinfo + 8);
                    if length < 0 { break; }
                    let start = offset + democmdinfo + 12;
                    let end = (start + length as usize).min(data.len());
                    packets += 1;
                    for m in scan_payload(&data[start..end]) {
                        total_msgs += 1;
                        let e = hist.entry(m.id).or_insert((0, 0));
                        e.0 += 1;
                        e.1 += m.body.len();
                        // Strongest alignment signal: each framed body must be
                        // walkable as protobuf to its end without a decode error.
                        let mut r = Reader::new(m.body);
                        loop {
                            match r.next_field() {
                                Ok(Some(_)) => {}
                                Ok(None) => break,
                                Err(_) => { bad_bodies += 1; break; }
                            }
                        }
                    }
                    offset = end;
                }
                4 => {                            // DEM_CONSOLECMD
                    if offset + 4 > data.len() { break; }
                    offset += 4 + le_i32(offset).max(0) as usize;
                }
                5 => {                            // DEM_USERCMD
                    if offset + 8 > data.len() { break; }
                    offset += 8 + le_i32(offset + 4).max(0) as usize;
                }
                6 => {                            // DEM_DATATABLES
                    if offset + 4 > data.len() { break; }
                    offset += 4 + le_i32(offset).max(0) as usize;
                }
                8 => {                            // DEM_CUSTOMDATA (proto-4)
                    if offset + 8 > data.len() { break; }
                    offset += 8 + le_i32(offset + 4).max(0) as usize;
                }
                9 => {                            // DEM_STRINGTABLES (proto-4)
                    if offset + 4 > data.len() { break; }
                    offset += 4 + le_i32(offset).max(0) as usize;
                }
                _ => break,
            }
        }

        let mut rows: Vec<_> = hist.iter().collect();
        rows.sort_by_key(|(id, _)| **id);
        eprintln!("\n=== CS:GO framing: {packets} packets, {total_msgs} messages ===");
        for (id, (count, bytes)) in rows {
            eprintln!("  {:>3} {:<24} x{:<6} {} bytes", id, MsgKind::from_id(*id).name(), count, bytes);
        }
        eprintln!("  malformed bodies: {bad_bodies}/{total_msgs}");

        // Assertions: we framed a real workload, svc_PacketEntities is present
        // (the message that carries positions), and every framed body parsed as
        // clean protobuf — i.e. the framing never desynced.
        assert!(total_msgs > 1000, "suspiciously few messages: {total_msgs}");
        assert!(hist.contains_key(&26), "no svc_PacketEntities (26) framed");
        assert_eq!(bad_bodies, 0, "{bad_bodies} framed bodies failed protobuf parse → framing desynced");
    }
}
