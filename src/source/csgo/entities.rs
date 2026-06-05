// CS:GO svc_PacketEntities → entity state, via protobuf `CSVCMsg_PacketEntities`.
//
// In CS:GO the entity-update bitstream that older engines write inline is moved
// into a protobuf field: `CSVCMsg_PacketEntities.entity_data` (field 7) carries
// the exact same bit-packed payload `parse_entity_updates` already decodes for
// the proto-4 family (Portal 2 / L4D). So once we pull the bytes (and the
// `updated_entries` / `is_delta` header fields) out of the protobuf envelope,
// the heavy lifting is the shared decoder — CS:GO is a proto-4 engine, so the
// `read_ubit_int` entity-index encoding and `CDeltaBitsReader` prop-index path
// are bit-for-bit what it uses.
//
// Field numbers from `CSVCMsg_PacketEntities` (netmessages.proto):
//   1:max_entries 2:updated_entries 3:is_delta 4:update_baseline
//   5:baseline 6:delta_from 7:entity_data
//
// Instance baselines (the `update_baseline` / string-table path) are NOT applied
// here: like the Portal 2 path, entering entities are decoded from the props
// present in the message. The networked origin is sent on entry and changes
// often, so position tracks come through without baseline reconstruction.

use super::super::datatable::DataTables;
use super::super::packetentities::{parse_entity_updates, EntityWorld};
use super::super::super::protobuf::Reader;

/// Decode one `CSVCMsg_PacketEntities` body into `world`. Returns `None` if the
/// protobuf envelope is malformed or the inner bitstream desyncs.
pub fn decode_packet_entities(body: &[u8], world: &mut EntityWorld, data: &DataTables) -> Option<()> {
    let mut r = Reader::new(body);
    let mut updated_entries: i32 = 0;
    let mut is_delta = false;
    let mut entity_data: &[u8] = &[];

    loop {
        let field = match r.next_field() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => return None,
        };
        match field.number {
            2 => updated_entries = field.value.as_i32()?,
            3 => is_delta = field.value.as_bool()?,
            7 => entity_data = field.value.as_bytes()?,
            _ => {} // max_entries / baseline / delta_from unused for tracking
        }
    }

    // The entity_data bytes are their own bit buffer: start at bit 0, span the
    // whole slice. CS:GO is proto-4 → proto4 = true.
    parse_entity_updates(
        entity_data,
        0,
        entity_data.len() * 8,
        updated_entries as u32,
        is_delta,
        world,
        data,
        true, // proto-4 entity-index + field-index encoding
        true, // CS:GO: separated index list then value list
        11,   // stock Source MAX_EDICT_BITS (CS:GO did not raise the edict limit)
        None, // prop-index encoding follows the proto-4 entity encoding
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::message::{scan_payload, MsgKind};
    use super::super::sendtable;

    // End-to-end Phase 3: build DataTables, decode every PacketEntities across a
    // real demo, and confirm the bitstream holds (no desync) + CCSPlayer entities
    // populate with real-looking world origins. Self-skips without CSGO_DEMO.
    //
    // The key CS:GO-specific detail (verified vs demoinfocs-golang's
    // Entity.ApplyUpdate): the field-index list and the value list are SEPARATED,
    // not interleaved like the Portal 2 path. Decoding them interleaved desynced
    // every message; reading all indices first, then all values, gives a 100%
    // decode rate and sane m_vecOrigin coordinates.
    //   CSGO_DEMO="DEMOS TESTING/monasterydemo.dem" cargo test --lib csgo_real_entities -- --nocapture
    #[test]
    fn csgo_real_entities() {
        let path = match std::env::var("CSGO_DEMO") {
            Ok(p) => p,
            Err(_) => { eprintln!("[skip] set CSGO_DEMO to run against a real demo"); return; }
        };
        let data = std::fs::read(&path).expect("read CSGO_DEMO");
        assert!(&data[0..8] == b"HL2DEMO\0");

        const HEADER: usize = 1072;
        let democmdinfo = 76 * 2;
        let pkt_hdr = 6;
        let le_i32 = |o: usize| i32::from_le_bytes(data[o..o + 4].try_into().unwrap());

        let mut tables: Option<DataTables> = None;
        let mut world: Option<EntityWorld> = None;
        let (mut ok, mut fail) = (0usize, 0usize);

        let mut offset = HEADER;
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
                    let start = offset + democmdinfo + 12;
                    let end = (start + len as usize).min(data.len());
                    if let (Some(dt), Some(w)) = (tables.as_ref(), world.as_mut()) {
                        for m in scan_payload(&data[start..end]) {
                            if m.kind == MsgKind::SvcPacketEntities {
                                match decode_packet_entities(m.body, w, dt) {
                                    Some(()) => ok += 1,
                                    None => fail += 1,
                                }
                            }
                        }
                    }
                    offset = end;
                }
                4 => { if offset + 4 > data.len() { break; } offset += 4 + le_i32(offset).max(0) as usize; }
                5 => { if offset + 8 > data.len() { break; } offset += 8 + le_i32(offset + 4).max(0) as usize; }
                6 => {
                    if offset + 4 > data.len() { break; }
                    let len = le_i32(offset).max(0) as usize;
                    let start = offset + 4;
                    let dt = sendtable::parse(&data[start..(start + len).min(data.len())])
                        .expect("DataTables parse");
                    world = Some(EntityWorld::new(&dt));
                    tables = Some(dt);
                    offset = start + len;
                }
                8 => { if offset + 8 > data.len() { break; } offset += 8 + le_i32(offset + 4).max(0) as usize; }
                9 => { if offset + 4 > data.len() { break; } offset += 4 + le_i32(offset).max(0) as usize; }
                _ => break,
            }
        }

        let dt = tables.expect("no DataTables");
        let w = world.expect("no world");
        let player_class = dt.server_classes.iter().find(|c| c.name == "CCSPlayer").unwrap().id;

        // Inspect the CCSPlayer entities we ended up with.
        let players: Vec<_> = w.entities.values().filter(|e| e.class_id == player_class).collect();
        eprintln!("\n=== CS:GO entities: {} PacketEntities ok, {} failed ===", ok, fail);
        eprintln!("    {} total entities, {} CCSPlayer", w.entities.len(), players.len());
        let flat = dt.flat_props.get(&player_class).unwrap();
        if let Some(p) = players.first() {
            for (idx, val) in p.props.iter() {
                let nm = &flat[*idx].name;
                if nm.contains("Origin") || nm.contains("cell") || nm.contains("Cell") {
                    eprintln!("    [{}] {:<14} = {:?}", idx, nm, val);
                }
            }
        }
        let rate = ok as f64 / (ok + fail).max(1) as f64;
        eprintln!("    decode success rate: {:.1}%", rate * 100.0);

        // The bitstream must hold across the whole demo and player entities must
        // populate — a single desync corrupts every later delta.
        assert!(rate > 0.95, "PacketEntities decode rate only {:.1}% — entity decode desyncs", rate * 100.0);
        assert!(!players.is_empty(), "no CCSPlayer entities decoded");
    }
}
