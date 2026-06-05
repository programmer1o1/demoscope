// CS:GO DEM_DATATABLES → DataTables, via protobuf `CSVCMsg_SendTable`.
//
// Where older Source engines bit-pack the SendTables (handled in
// `datatable::parse`), CS:GO encodes each one as a protobuf `CSVCMsg_SendTable`
// message. The DEM_DATATABLES payload is a sequence of
//   varint(type=svc_SendTable) varint(size) <CSVCMsg_SendTable body>
// repeated until a table with `is_end = true`, immediately followed by the
// server-class list as raw bytes:
//   u16 count, then per class { u16 id, cstring class_name, cstring dt_name }.
//
// We decode the messages into the *same* `RawSendTable` / `ServerClass` structs
// the bit-packed path builds, then hand them to `datatable::build_data_tables`,
// so the flatten + prop-decode machinery is shared verbatim.
//
// Field numbers come from CS:GO's `netmessages.proto` (`CSVCMsg_SendTable` and
// its nested `sendprop_t`), stable across the protocol-13xxx era:
//   CSVCMsg_SendTable { 1:is_end 2:net_table_name 3:needs_decoder 4:props[] }
//   sendprop_t { 1:type 2:var_name 3:flags 4:priority 5:dt_name
//                6:num_elements 7:low_value 8:high_value 9:num_bits }

use super::super::datatable::{self, RawSendPropDef, RawSendTable, ServerClass, DataTables};
use super::super::sendprop::{SendPropType, SPROP_EXCLUDE, SPROP_INSIDEARRAY, SPROP_NORMAL_OR_VARINT};
use super::super::super::protobuf::Reader;

/// CS:GO's `SPROP_VARINT` sits at bit 19 in the modern flag layout (it post-dates
/// the 16-bit TF2 space), so `normalize_portal2_flags` drops it. demoscope's int
/// decoder reads varint off the overloaded bit-5 (`SPROP_NORMAL_OR_VARINT`), so
/// we re-fold a set bit 19 onto bit 5. VARINT only ever applies to int props, so
/// this can't be mistaken for a float's NORMAL flag.
const SPROP_VARINT_BIT19: u32 = 1 << 19;

/// Decode a CS:GO DEM_DATATABLES payload into a flattened `DataTables`.
/// Returns `None` on a malformed/truncated stream (caller leaves entities off,
/// same as a failed bit-packed parse).
pub fn parse(payload: &[u8]) -> Option<DataTables> {
    let mut reader = Reader::new(payload);
    let mut tables: Vec<RawSendTable> = Vec::new();

    // Frame and decode CSVCMsg_SendTable messages until the is_end sentinel.
    loop {
        if reader.is_empty() {
            return None; // ran out before the is_end table — malformed
        }
        let _msg_type = reader.read_varint().ok()?; // svc_SendTable (9); not enforced
        let size = reader.read_varint().ok()? as usize;
        let body = reader.read_bytes(size).ok()?;
        let (table, is_end) = decode_send_table(body)?;
        if is_end {
            break;
        }
        tables.push(table);
    }

    // The remaining bytes are the byte-aligned server-class list.
    let server_classes = read_server_classes(reader.remaining())?;

    // CS:GO is demo_protocol 4 → proto4 flatten path.
    Some(datatable::build_data_tables(tables, server_classes, true))
}

/// Decode one `CSVCMsg_SendTable` body. Returns the table plus its `is_end` flag.
fn decode_send_table(body: &[u8]) -> Option<(RawSendTable, bool)> {
    let mut r = Reader::new(body);
    let mut is_end = false;
    let mut name = String::new();
    let mut needs_decoder = false;
    let mut raw_props: Vec<RawSendPropDef> = Vec::new();

    loop {
        let field = match r.next_field() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => return None,
        };
        match field.number {
            1 => is_end = field.value.as_bool()?,
            2 => name = field.value.as_str()?.into_owned(),
            3 => needs_decoder = field.value.as_bool()?,
            4 => {
                if let Some(prop) = decode_prop(field.value.as_bytes()?) {
                    raw_props.push(prop);
                }
            }
            _ => {} // forward-compatible: ignore unknown fields
        }
    }

    Some((RawSendTable { needs_decoder, name, props: bind_array_elements(raw_props) }, is_end))
}

/// Decode one nested `sendprop_t` into a `RawSendPropDef`.
fn decode_prop(body: &[u8]) -> Option<RawSendPropDef> {
    let mut r = Reader::new(body);
    let mut type_raw = 0i32;
    let mut name = String::new();
    let mut raw_flags = 0u32;
    let mut priority = 0i32;
    let mut dt_name = String::new();
    let mut num_elements = 0i32;
    let mut low_value = 0.0f32;
    let mut high_value = 0.0f32;
    let mut num_bits = 0i32;

    loop {
        let field = match r.next_field() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => return None,
        };
        match field.number {
            1 => type_raw = field.value.as_i32()?,
            2 => name = field.value.as_str()?.into_owned(),
            3 => raw_flags = field.value.as_u32()?,
            4 => priority = field.value.as_i32()?,
            5 => dt_name = field.value.as_str()?.into_owned(),
            6 => num_elements = field.value.as_i32()?,
            7 => low_value = field.value.as_f32()?,
            8 => high_value = field.value.as_f32()?,
            9 => num_bits = field.value.as_i32()?,
            _ => {}
        }
    }

    let prop_type = SendPropType::from_u8(type_raw as u8)?;
    // CS:GO encodes flags in the modern (Alien Swarm) 19-bit layout — the same
    // one the Portal 2 bit-packed path normalises to canonical TF2 positions.
    let mut flags = datatable::normalize_portal2_flags(raw_flags);
    if raw_flags & SPROP_VARINT_BIT19 != 0 {
        flags |= SPROP_NORMAL_OR_VARINT; // bit 5 → decode this int as a varint
    }

    let (exclude_dt_name, data_table_name) = if flags & SPROP_EXCLUDE != 0 {
        (Some(dt_name), None)
    } else if prop_type == SendPropType::DataTable {
        (None, Some(dt_name))
    } else {
        (None, None)
    };

    Some(RawSendPropDef {
        prop_type,
        name,
        flags,
        priority: priority as u8,
        exclude_dt_name,
        data_table_name,
        low_value,
        high_value,
        bit_count: num_bits as u32,
        element_count: num_elements as u16,
        array_element: None, // bound in bind_array_elements
    })
}

/// Pair each Array prop with the `InsideArray` element definition that precedes
/// it — mirrors the bit-packed `read_send_table` logic so flatten sees the same
/// shape regardless of wire format.
fn bind_array_elements(raw_props: Vec<RawSendPropDef>) -> Vec<RawSendPropDef> {
    let mut array_element: Option<RawSendPropDef> = None;
    let mut out = Vec::with_capacity(raw_props.len());
    for prop in raw_props {
        if prop.flags & SPROP_INSIDEARRAY != 0 {
            array_element = Some(prop);
        } else if prop.prop_type == SendPropType::Array {
            if let Some(elem) = array_element.take() {
                let mut bound = prop;
                bound.array_element = Some(Box::new(elem));
                out.push(bound);
            }
            // An Array prop with no pending element is malformed; drop it.
        } else {
            out.push(prop);
        }
    }
    out
}

/// Read the trailing server-class table: `u16 count`, then per class a `u16 id`
/// and two NUL-terminated strings (class name, data-table name).
fn read_server_classes(bytes: &[u8]) -> Option<Vec<ServerClass>> {
    let count = u16::from_le_bytes(bytes.get(0..2)?.try_into().ok()?) as usize;
    let mut pos = 2usize;
    let mut classes = Vec::with_capacity(count);
    for _ in 0..count {
        let id = u16::from_le_bytes(bytes.get(pos..pos + 2)?.try_into().ok()?);
        pos += 2;
        let (name, np) = read_cstr(bytes, pos)?;
        pos = np;
        let (dt, np2) = read_cstr(bytes, pos)?;
        pos = np2;
        classes.push(ServerClass { id, name, data_table: dt });
    }
    Some(classes)
}

/// Read a NUL-terminated string starting at `pos`; returns the string and the
/// index just past the terminator.
fn read_cstr(bytes: &[u8], pos: usize) -> Option<(String, usize)> {
    let rest = bytes.get(pos..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    let s = String::from_utf8_lossy(&rest[..end]).into_owned();
    Some((s, pos + end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Decode the DEM_DATATABLES out of a real CS:GO demo and sanity-check the
    // server-class list + flattened player props. Self-skips without CSGO_DEMO.
    //   CSGO_DEMO="DEMOS TESTING/monasterydemo.dem" cargo test --lib csgo_real_sendtables -- --nocapture
    #[test]
    fn csgo_real_sendtables() {
        let path = match std::env::var("CSGO_DEMO") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("[skip] set CSGO_DEMO to run against a real demo");
                return;
            }
        };
        let data = std::fs::read(&path).expect("read CSGO_DEMO");
        assert!(&data[0..8] == b"HL2DEMO\0");

        const HEADER: usize = 1072;
        let democmdinfo = 76 * 2;
        let pkt_hdr = 6;
        let le_i32 = |o: usize| i32::from_le_bytes(data[o..o + 4].try_into().unwrap());

        // Walk the container to the DEM_DATATABLES (cmd 6) payload.
        let mut offset = HEADER;
        let mut dt_payload: Option<&[u8]> = None;
        while offset + pkt_hdr <= data.len() {
            let raw_cmd = data[offset];
            offset += pkt_hdr;
            match raw_cmd {
                7 => break,
                3 => {}
                1 | 2 => {
                    if offset + democmdinfo + 12 > data.len() { break; }
                    let len = le_i32(offset + democmdinfo + 8);
                    if len < 0 { break; }
                    offset += democmdinfo + 12 + len as usize;
                }
                4 => { if offset + 4 > data.len() { break; } offset += 4 + le_i32(offset).max(0) as usize; }
                5 => { if offset + 8 > data.len() { break; } offset += 8 + le_i32(offset + 4).max(0) as usize; }
                6 => {
                    if offset + 4 > data.len() { break; }
                    let len = le_i32(offset).max(0) as usize;
                    let start = offset + 4;
                    dt_payload = Some(&data[start..(start + len).min(data.len())]);
                    break;
                }
                8 => { if offset + 8 > data.len() { break; } offset += 8 + le_i32(offset + 4).max(0) as usize; }
                9 => { if offset + 4 > data.len() { break; } offset += 4 + le_i32(offset).max(0) as usize; }
                _ => break,
            }
        }

        let payload = dt_payload.expect("no DEM_DATATABLES found");
        let dt = parse(payload).expect("CS:GO SendTable parse failed");

        eprintln!(
            "\n=== CS:GO DataTables: {} send tables, {} server classes, {} flattened ===",
            dt.tables.len(), dt.server_classes.len(), dt.flat_props.len()
        );
        // Show a few recognisable classes + the player class's flat prop count.
        for c in &dt.server_classes {
            if matches!(c.name.as_str(), "CCSPlayer" | "CWorld" | "CCSPlayerResource" | "CCSTeam") {
                let n = dt.flat_props.get(&c.id).map(|p| p.len()).unwrap_or(0);
                eprintln!("  id={:<4} {:<22} dt={:<26} flat_props={}", c.id, c.name, c.data_table, n);
            }
        }

        // A real CS:GO table set has a couple hundred classes; the player class
        // must exist and flatten to a non-trivial prop list (m_vecOrigin lives there).
        assert!(dt.server_classes.len() > 100, "too few classes: {}", dt.server_classes.len());
        let player = dt.server_classes.iter().find(|c| c.name == "CCSPlayer")
            .expect("no CCSPlayer class");
        let player_props = dt.flat_props.get(&player.id).expect("CCSPlayer not flattened");
        assert!(player_props.len() > 50, "CCSPlayer flattened to only {} props", player_props.len());
        // The networked origin prop should be somewhere in the flattened list.
        assert!(
            player_props.iter().any(|p| p.name.contains("m_vecOrigin") || p.name == "m_cellX"),
            "CCSPlayer flat props missing an origin prop"
        );
    }
}
