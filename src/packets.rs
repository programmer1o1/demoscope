// Demo command-stream walkers that work at the packet level (not the inner
// net-message bitstream): the splitscreen-slot detector every walker needs to
// size democmdinfo, the packet iterator, the userinfo string-table parser, and
// the svc_SetView spectator-switch extraction.

use std::collections::HashMap;

use super::bitreader::BitReader;
use super::bytes::le_i32;
use super::constants::{HEADER_SIZE, SPLIT_SIZE};

pub(crate) struct DemoPacketInfo {
    pub(crate) cmd: u8,
    pub(crate) tick: i32,
    pub(crate) payload_start: usize,
    pub(crate) payload_end: usize,
}

// Proto-4's democmdinfo is an array of Split_t[MAX_SPLITSCREEN_CLIENTS], each
// Split_t being 76 bytes (flags(4) + 6 × vec3(12)). The slot count varies by
// game: L4D1/L4D2 ship 4, Portal 2/Stanley/CS:GO 2, most others 1. Detect it by
// probing the first SIGNON/PACKET frame: for each candidate N, read the length
// field that *would* sit at democmdinfo(76*N)+8 and accept the first N whose
// length reads as a sane payload size. demo_protocol <= 3 has no Split_t array.
//
// Every code path that walks the demo command stream MUST size democmdinfo with
// this - hardcoding 76 (splitscreen=1) desyncs L4D right after the first signon.
pub(crate) fn detect_splitscreen(data: &[u8], demo_protocol: i32, game_dir: &str) -> usize {
    // Portal 2-engine games (Portal 2, Stanley, …) always ship 2 splitscreen
    // slots. Pin them rather than probe: the [4,2,1] length-probe below can
    // false-positive as 4 on these (a puzzlemaker export did), which desyncs
    // the cmd=1/2 payload boundaries and silently drops every game packet -
    // breaking event/userinfo/setview extraction. player_tracks pins the same
    // way; L4D (splitscreen 4) is NOT portal2-engine, so it still probes.
    if demo_protocol > 3 && super::source_demo::datatable::is_portal2_engine(game_dir) {
        return 2;
    }
    let extra: usize = if demo_protocol > 3 { 1 } else { 0 };
    let pkt_hdr = 5 + extra;
    if demo_protocol > 3 && data.len() > HEADER_SIZE + pkt_hdr + 100 {
        let pkt_start = HEADER_SIZE + pkt_hdr;
        for n in [4, 2, 1] {
            let len_off = pkt_start + SPLIT_SIZE * n + 8;
            if len_off + 4 > data.len() { continue; }
            let length = le_i32(data, len_off);
            if length <= 0 { continue; }
            let payload_end = len_off.saturating_add(4).saturating_add(length as usize);
            if (length as usize) < (data.len() - pkt_start) && payload_end < data.len() {
                return n;
            }
        }
    }
    1
}

pub(crate) fn iterate_demo_packets(data: &[u8], demo_protocol: i32, game_dir: &str) -> Vec<DemoPacketInfo> {
    // demo_protocol > 3 (L4D, Portal 2, CS:GO, …) adds a player_slot byte after cmd+tick
    let extra: usize = if demo_protocol > 3 { 1 } else { 0 };
    let pkt_hdr = 5 + extra;
    let democmdinfo = SPLIT_SIZE * detect_splitscreen(data, demo_protocol, game_dir);
    let preamble = democmdinfo + 12;

    let mut packets = Vec::new();
    let mut offset = HEADER_SIZE;

    while offset < data.len() {
        if offset + 5 > data.len() { break; }
        let cmd = data[offset];
        let tick = le_i32(data, offset + 1);
        offset += pkt_hdr;

        match cmd {
            3 => {
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
            }
            7 => {
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
                break;
            }
            1 | 2 => {
                if offset + preamble > data.len() { break; }
                // A negative length is desync garbage (and would overflow usize
                // on the add in a debug build); treat it as end-of-stream.
                let length = le_i32(data, offset + democmdinfo + 8);
                if length < 0 { break; }
                let payload_start = offset + preamble;
                let payload_end = payload_start.saturating_add(length as usize);
                if payload_end > data.len() { break; }
                packets.push(DemoPacketInfo { cmd, tick, payload_start, payload_end });
                offset = payload_end;
            }
            4 => {
                // ConsoleCmd
                if offset + 4 > data.len() { break; }
                let length = le_i32(data, offset);
                if length < 0 { break; }
                let next = offset.saturating_add(4).saturating_add(length as usize);
                if next > data.len() { break; }
                offset = next;
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
            }
            5 => {
                // UserCmd
                if offset + 8 > data.len() { break; }
                let length = le_i32(data, offset + 4);
                if length < 0 { break; }
                let next = offset.saturating_add(8).saturating_add(length as usize);
                if next > data.len() { break; }
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
                offset = next;
            }
            8 if demo_protocol > 3 => {
                // Proto-4 DEM_CUSTOMDATA: id(4) + length(4) + data[length]. Its
                // 8-byte header differs from the plain length-prefixed commands
                // below; lumping it in with them reads the id as the length and
                // desyncs the whole walk (this dropped every game packet on
                // Portal 2, which emits a CustomData right after the signon).
                // Matches the cmd-99 handler in player_tracks::scan.
                if offset + 8 > data.len() { break; }
                let length = le_i32(data, offset + 4);
                if length < 0 { break; }
                let next = offset.saturating_add(8).saturating_add(length as usize);
                if next > data.len() { break; }
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
                offset = next;
            }
            6 | 8 | 9 => {
                // 6=DataTables, 8=StringTables(proto-3), 9=StringTables(proto-4)
                if offset + 4 > data.len() { break; }
                let length = le_i32(data, offset);
                if length < 0 { break; }
                let next = offset.saturating_add(4).saturating_add(length as usize);
                if next > data.len() { break; }
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
                offset = next;
            }
            _ => break,
        }
    }

    packets
}

pub(crate) fn parse_userinfo_from_demo(data: &[u8], proto: i32, game_dir: &str) -> (HashMap<i32, (String, bool)>, HashMap<usize, i32>) {
    let extra: usize = if proto > 3 { 1 } else { 0 };
    let pkt_hdr = 5 + extra;
    let democmdinfo = SPLIT_SIZE * detect_splitscreen(data, proto, game_dir); // L4D = 4 slots
    let mut offset = HEADER_SIZE;
    let mut result = HashMap::new();
    let mut slot_to_uid: HashMap<usize, i32> = HashMap::new();

    while offset < data.len() {
        if offset + 5 > data.len() { break; }
        let cmd = data[offset];
        offset += pkt_hdr;

        match cmd {
            7 => break,
            1 | 2 => {
                if offset + democmdinfo + 12 > data.len() { break; }
                let length = le_i32(data, offset + democmdinfo + 8);
                if length < 0 { break; }
                offset = offset.saturating_add(democmdinfo + 12).saturating_add(length as usize);
            }
            3 => {}
            4 => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(data, offset);
                if length < 0 { break; }
                offset = offset.saturating_add(4).saturating_add(length as usize);
            }
            5 => {
                if offset + 8 > data.len() { break; }
                let length = le_i32(data, offset + 4);
                if length < 0 { break; }
                offset = offset.saturating_add(8).saturating_add(length as usize);
            }
            8 if proto > 3 => {
                // Proto-4 DEM_CUSTOMDATA: id(4) + length(4) + data. NOT a string
                // table - skip its 8-byte header cleanly so the walk stays
                // aligned and the real userinfo (cmd 9) is reached. (StringTables
                // moved to cmd 9 in the proto-4 enum.)
                if offset + 8 > data.len() { break; }
                let length = le_i32(data, offset + 4);
                if length < 0 { break; }
                offset = offset.saturating_add(8).saturating_add(length as usize);
            }
            // StringTables is cmd 8 (old enum: Portal 2/Orange Box proto-3) or
            // cmd 9 (new enum: L4D/CS:GO proto-4). userinfo lives in that block.
            6 | 8 | 9 => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(data, offset);
                if length < 0 { break; }
                let payload_start = offset + 4;
                let payload_end = payload_start.saturating_add(length as usize).min(data.len());
                if (cmd == 8 || cmd == 9) && payload_end > payload_start {
                    if let Some((m, s)) = try_parse_userinfo_tables(&data[payload_start..payload_end]) {
                        result.extend(m);
                        slot_to_uid.extend(s);
                    }
                }
                offset = payload_end;
            }
            _ => break,
        }
    }
    (result, slot_to_uid)
}

// (name, is_fake)  - is_fake = player_info_t.fakeplayer (offset 124)
fn try_parse_userinfo_tables(payload: &[u8]) -> Option<(HashMap<i32, (String, bool)>, HashMap<usize, i32>)> {
    let mut br = BitReader::new(payload);
    let num_tables = br.read_bits(8)? as usize;
    if num_tables == 0 || num_tables > 64 { return None; }
    let mut result = HashMap::new();
    let mut slot_to_uid: HashMap<usize, i32> = HashMap::new();

    for _t in 0..num_tables {
        let table_name = br.read_cstring_any(64)?;
        let num_strings = br.read_bits(16)? as usize;
        if num_strings > 4096 { return None; }
        let is_userinfo = table_name == "userinfo";

        for slot in 0..num_strings {
            let player_name = br.read_cstring_any(512)?;
            let has_ud = br.read_bits(1)? != 0;
            let mut uid: Option<i32> = None;
            let mut is_fake = false;
            if has_ud {
                let ud_len = br.read_bits(16)? as usize;
                if ud_len > 8192 { return None; }
                if is_userinfo && ud_len >= 52 {
                    let mut bytes = vec![0u8; ud_len];
                    for i in 0..ud_len { bytes[i] = br.read_bits(8)? as u8; }
                    // player_info_t: version(8)+xuid(8)+name(32)=48, userID(4) at 48
                    uid = Some(i32::from_le_bytes([bytes[48], bytes[49], bytes[50], bytes[51]]));
                    // fakeplayer at offset 124 (after guid[33]+pad+friendsID+friendsName)
                    if ud_len > 124 { is_fake = bytes[124] != 0; }
                    // actual player name is at bytes[16..48] (after version+xuid), null-terminated
                    let name_end = bytes[16..48.min(ud_len)].iter().position(|&b| b == 0).unwrap_or(32);
                    let actual_name = String::from_utf8_lossy(&bytes[16..16 + name_end]).into_owned();
                    if !actual_name.is_empty() {
                        let id = uid.unwrap_or(slot as i32 + 1);
                        if id > 0 {
                            result.insert(id, (actual_name, is_fake));
                            slot_to_uid.insert(slot, id);
                        }
                    }
                    continue;
                } else {
                    for _ in 0..ud_len { br.read_bits(8)?; }
                }
            }
            if is_userinfo && !player_name.is_empty() {
                let id = uid.unwrap_or(slot as i32 + 1);
                if id > 0 {
                    result.insert(id, (player_name, is_fake));
                    slot_to_uid.insert(slot, id);
                }
            }
        }

        let has_cs = br.read_bits(1)? != 0;
        if has_cs {
            let n = br.read_bits(16)? as usize;
            for _ in 0..n {
                br.read_cstring_any(512)?;
                let hud = br.read_bits(1)? != 0;
                if hud {
                    let l = br.read_bits(16)? as usize;
                    for _ in 0..l { br.read_bits(8)?; }
                }
            }
        }
    }
    if result.is_empty() { None } else { Some((result, slot_to_uid)) }
}

// ── svc_SetView extraction ────────────────────────────────────────────────────
// Parses each game-packet payload looking for svc_SetView (net message type 17
// in TF2's demo protocol, immediately after net_Tick type 3). Returns
// (tick, entity_index) pairs for every packet where svc_SetView was found.
pub(crate) fn extract_svc_setview(data: &[u8], game_packet_ticks: &[(i32, usize, usize)]) -> Vec<(i32, u16)> {
    let mut results = Vec::new();
    for &(tick, payload_start, payload_end) in game_packet_ticks {
        if payload_end <= payload_start { continue; }
        let payload = &data[payload_start..payload_end];
        let total_bits = payload.len() * 8;

        let read_bits = |pos: usize, n: usize| -> Option<u32> {
            if pos + n > total_bits { return None; }
            let mut v = 0u32;
            for i in 0..n {
                let bi = (pos + i) >> 3;
                v |= (((payload[bi] >> ((pos + i) & 7)) & 1) as u32) << i;
            }
            Some(v)
        };

        let mut bit_pos = 0usize;
        for _ in 0..15 {
            if bit_pos + 6 > total_bits { break; }
            let msg_type = match read_bits(bit_pos, 6) { Some(t) => t as u8, None => break };
            bit_pos += 6;
            match msg_type {
                0 => {}                  // net_NOP - no data
                3 => { bit_pos += 64; } // net_Tick - skip 64 bits
                17 => {                  // svc_SetView - 12-bit entity index
                    if let Some(entity) = read_bits(bit_pos, 12) {
                        results.push((tick, entity as u16));
                    }
                    break;
                }
                26 => break, // svc_PacketEntities - no SetView before entity updates
                _ => break,  // unknown message, stop
            }
        }
    }
    results
}

// Identify spectator-switch intervals: tick ranges [start, end) where the
// spectator was watching a non-primary entity. The primary entity is the one
// seen in the most svc_SetView events.
pub(crate) fn spectator_switch_intervals(setview_events: &[(i32, u16)]) -> Vec<(i32, i32)> {
    if setview_events.len() < 5 { return vec![]; }
    let mut counts = std::collections::HashMap::<u16, usize>::new();
    for &(_, e) in setview_events { *counts.entry(e).or_insert(0) += 1; }
    let primary = counts.into_iter().max_by_key(|&(_, c)| c).map(|(e, _)| e).unwrap_or(0);

    let mut intervals: Vec<(i32, i32)> = Vec::new();
    let mut switch_start: Option<i32> = None;
    let mut cur_entity: Option<u16> = None;

    for &(tick, entity) in setview_events {
        if cur_entity == Some(entity) { continue; }
        if let (Some(start), Some(prev)) = (switch_start, cur_entity) {
            if prev != primary { intervals.push((start, tick)); }
        }
        switch_start = Some(tick);
        cur_entity = Some(entity);
    }
    if let (Some(start), Some(e)) = (switch_start, cur_entity) {
        if e != primary {
            let end = setview_events.last().map(|&(t, _)| t + 1).unwrap_or(start + 1);
            intervals.push((start, end));
        }
    }
    intervals
}
