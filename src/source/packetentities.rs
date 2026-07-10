// svc_PacketEntities (net message type 26) decoder.
//
// Wire format (matches tf-demo-parser exactly):
//   For each updated entity:
//     1. bit_var: entity-index delta (2-bit type {0→4, 1→8, 2→12, 3→32 bits payload})
//     2. 2-bit update_type: {00=Delta, 01=Leave, 10=Enter, 11=Delete}
//     3. If Enter: class_id (class_bits) + serial (10 bits) + props
//     4. If Delta: props only
//   Then prop-update loop, terminated by a 0 stop-bit:
//     1. 1-bit "has-next"
//     2. bit_var: prop-index delta
//     3. SendProp value
//
// After the entity loop, if `delta` was set in the message header, a
// removed-entities list follows (1-bit + 11-bit eid pairs, 0 ends).

use std::collections::HashMap;

use super::bitreader::BitReader;
use super::datatable::DataTables;
use super::sendprop::{PropValue, SendPropDef, decode_prop};

#[derive(Debug, Clone)]
pub struct EntityState {
    pub class_id: u16,
    pub props: HashMap<usize, PropValue>,
}

#[derive(Default, Clone)]
pub struct EntityWorld {
    pub entities: HashMap<u16, EntityState>,
    pub class_bits: u32, // bits needed to encode class_id
}

impl EntityWorld {
    pub fn new(data: &DataTables) -> Self {
        // Source SDK formula: server_class_bits = floor(log2(num_classes)) + 1.
        // Match tf-demo-parser's `log_base2(n) as usize + 1` exactly.
        let n = data.server_classes.len().max(1);
        let log2 = 31 - (n as u32).leading_zeros();
        let bits = log2 + 1;
        EntityWorld {
            entities: HashMap::new(),
            class_bits: bits.max(1),
        }
    }
}

/// Old-engine (net_protocol <= 7) svc_PacketEntities body decoder.
///
/// Entity header (confirmed against the November 2004 retail `engine.dll`):
///   entity = last + 1 + ReadUBitVarOld()
///   1 bit leave-PVS:
///     leave=1: 1 bit delete flag
///     leave=0: 1 bit enter-PVS (0 = in-place delta)
///   ENTER: class_id(class_bits) + serial(10) + props
///   DELTA: props
///
/// `ReadUBitVarOld` is the engine's unsigned Exp-Golomb-style code: count zero
/// bits up to the first one, then read that many payload bits and add 2^n-1.
/// Property indices use the same code after a one-bit has-more marker.
pub fn parse_entity_updates_old(
    payload: &[u8],
    start_bit: usize,
    length_bits: usize,
    num_updates: u32,
    _has_delta_tick: bool,
    world: &mut EntityWorld,
    data: &DataTables,
    edict_bits: u32,
) -> Option<()> {
    let max_edicts: i32 = 1 << edict_bits;
    let mut br = BitReader::new_at(payload, start_bit);
    let end_bit = start_bit + length_bits;
    br.set_max_bit(end_bit);

    let dbg = std::env::var("DUMP_ENT2").is_ok();
    let mut entity_idx: i32 = -1;
    for _u in 0..num_updates {
        if br.bit_pos() >= end_bit {
            break;
        }
        let diff = read_ubit_var_old(&mut br)? as i32;
        entity_idx = entity_idx.checked_add(diff)?.checked_add(1)?;
        if entity_idx < 0 || entity_idx >= max_edicts {
            return None;
        }
        let eid = entity_idx as u16;

        let leave = br.read_bool()?;
        if leave {
            let delete = br.read_bool()?;
            if delete {
                world.entities.remove(&eid);
            }
            if dbg && _u < 12 {
                eprintln!("[ENT2-OLD] #{} eid={} LEAVE delete={}", _u, eid, delete);
            }
            continue;
        }

        let enter = br.read_bool()?;
        if enter {
            let class_id = br.read_bits(world.class_bits)? as u16;
            let _serial = br.read_bits(10)?;
            if dbg && _u < 12 {
                let nm = data
                    .server_classes
                    .iter()
                    .find(|c| c.id == class_id)
                    .map(|c| c.name.as_str())
                    .unwrap_or("?");
                eprintln!(
                    "[ENT2-OLD] #{} eid={} ENTER class={} ({})",
                    _u, eid, class_id, nm
                );
            }
            let mut state = EntityState {
                class_id,
                props: HashMap::new(),
            };
            // An unknown class id is impossible in a valid packet. Treat it as
            // a packet failure so the caller can discard the transactional copy
            // instead of poisoning the persistent entity world.
            let flat = data.flat_props.get(&class_id)?;
            read_prop_deltas_old(&mut br, flat, &mut state.props)?;
            world.entities.insert(eid, state);
        } else {
            // In-place delta against known state.
            let cid = match world.entities.get(&eid).map(|s| s.class_id) {
                Some(c) => c,
                None => break,
            };
            let flat = match data.flat_props.get(&cid) {
                Some(f) => f,
                None => break,
            };
            let state = world.entities.get_mut(&eid).unwrap();
            read_prop_deltas_old(&mut br, flat, &mut state.props)?;
        }
    }
    Some(())
}

/// November 2004 `CDeltaBitsReader::ReadNextPropIndex`.
fn read_prop_deltas_old(
    br: &mut BitReader,
    flat: &[SendPropDef],
    out: &mut HashMap<usize, PropValue>,
) -> Option<()> {
    let dbg = std::env::var("DUMP_PROPS").is_ok();
    let mut idx: i32 = -1;
    while br.read_bool()? {
        let diff = read_ubit_var_old(br)? as i32;
        idx = idx.checked_add(diff)?.checked_add(1)?;
        if idx < 0 || idx as usize >= flat.len() {
            if dbg {
                eprintln!("[PROP-OLD] idx={} OUT OF RANGE (len {})", idx, flat.len());
            }
            return None;
        }
        let prop = &flat[idx as usize];
        let val = decode_prop(prop, br)?;
        if dbg {
            eprintln!(
                "[PROP-OLD] {:>3} {}.{} {:?} flags=0x{:x} bits={} = {:?}",
                idx, prop.table_name, prop.name, prop.prop_type, prop.flags, prop.bit_count, val
            );
        }
        out.insert(idx as usize, val);
    }
    Some(())
}

/// Retail Source 2004 `bf_read::ReadUBitVar`: unsigned Exp-Golomb-style code.
fn read_ubit_var_old(br: &mut BitReader) -> Option<u32> {
    let mut zeros = 0u32;
    while !br.read_bool()? {
        zeros += 1;
        if zeros >= 32 {
            return None;
        }
    }
    let base = (1u32 << zeros) - 1;
    let payload = if zeros == 0 { 0 } else { br.read_bits(zeros)? };
    base.checked_add(payload)
}

#[cfg(test)]
mod old_engine_tests {
    use super::{BitReader, read_ubit_var_old};

    #[test]
    fn retail_2004_ubit_var_roundtrips_gaps() {
        let values = [0u32, 1, 2, 3, 4, 7, 15, 31, 255];
        let mut bits = Vec::new();
        for value in values {
            let encoded = value + 1;
            let width = 31 - encoded.leading_zeros();
            bits.extend(std::iter::repeat_n(false, width as usize));
            bits.push(true);
            let payload = value - ((1u32 << width) - 1);
            for bit in 0..width {
                bits.push(payload & (1 << bit) != 0);
            }
        }

        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (index, bit) in bits.into_iter().enumerate() {
            if bit {
                bytes[index / 8] |= 1 << (index % 8);
            }
        }

        let mut reader = BitReader::new(&bytes);
        for expected in values {
            assert_eq!(read_ubit_var_old(&mut reader), Some(expected));
        }
    }
}

/// Read Source's 2-bit-type variable-length integer:
///   type 0 → 4 bits payload
///   type 1 → 8 bits payload
///   type 2 → 12 bits payload
///   type 3 → 32 bits payload
fn read_bit_var(br: &mut BitReader) -> Option<u32> {
    let ty = br.read_bits(2)?;
    let bits = match ty {
        0 => 4,
        1 => 8,
        2 => 12,
        _ => 32,
    };
    br.read_bits(bits)
}

/// Process a single svc_PacketEntities message body.
///
/// `payload_bits` is the entity-updates section length (the `length` field
/// from the message header). `num_updates` is the `updated_entries` count.
/// `has_delta_tick` controls whether the removed-entities list is appended.
#[allow(clippy::too_many_arguments)]
pub fn parse_entity_updates(
    payload: &[u8],
    start_bit: usize,
    length_bits: usize,
    num_updates: u32,
    has_delta_tick: bool,
    world: &mut EntityWorld,
    data: &DataTables,
    proto4: bool,
    separated: bool,
    // MAX_EDICT_BITS: 11 for stock Source (2048 edicts), 13 for GMod 13 (8192).
    // Bounds the entity-index space and the removed-entities list field width.
    edict_bits: u32,
    // Field-index (prop) encoding selector, decoupled from the entity-index
    // encoding (`proto4`). GMod 13 keeps the legacy 2-bit-selector entity index
    // but uses the newer CDeltaBitsReader prop-index path, so these two axes are
    // independent. When None, falls back to `proto4`.
    prop_proto4: Option<bool>,
) -> Option<()> {
    let max_edicts: i32 = 1 << edict_bits;
    let prop_proto4 = prop_proto4.unwrap_or(proto4);
    // Construct a bit reader limited to exactly the entity-updates section.
    // Reads past the message boundary now return None instead of trampling
    // subsequent messages - matches tf-demo-parser's read_bits(length) split.
    let mut br = BitReader::new_at(payload, start_bit);
    let end_bit = start_bit + length_bits;
    br.set_max_bit(end_bit);

    let dbg = std::env::var("DUMP_ENT2").is_ok();
    let mut entity_idx: i32 = -1;
    for _u in 0..num_updates {
        if br.bit_pos() >= end_bit {
            break;
        }

        // Entity-index delta. Proto-4 (Portal 2 / Source 2009 era) uses the
        // 6-bit ReadUBitInt (confirmed in engine.dll CEntityReadInfo via IDA:
        // 6-bit base, low 4 kept, extension 4/8/28 << 4). Proto-3 (TF2/CS:S)
        // uses the 2-bit-selector UBitVar.
        let diff = if proto4 {
            br.read_ubit_int()? as i32
        } else {
            read_bit_var(&mut br)? as i32
        };
        entity_idx = entity_idx.saturating_add(diff).saturating_add(1);
        if entity_idx < 0 || entity_idx >= max_edicts {
            return None;
        }
        let eid = entity_idx as u16;

        let update_type = br.read_bits(2)?;
        if dbg && _u < 12 {
            eprintln!(
                "[ENT2] #{} eid={} type={:02b} class_bits={} bit_pos={}",
                _u,
                eid,
                update_type,
                world.class_bits,
                br.bit_pos()
            );
        }
        match update_type {
            // 10 = Enter PVS - class id + serial + props
            0b10 => {
                let class_id = br.read_bits(world.class_bits)? as u16;
                let _serial = br.read_bits(10)?;
                if dbg && _u < 12 {
                    let nm = data
                        .flat_props
                        .get(&class_id)
                        .map(|f| f.len())
                        .unwrap_or(usize::MAX);
                    eprintln!(
                        "[ENT2]    ENTER class_id={} flat_len={}",
                        class_id, nm as i64
                    );
                }
                let mut state = EntityState {
                    class_id,
                    props: HashMap::new(),
                };
                if let Some(flat) = data.flat_props.get(&class_id) {
                    read_prop_deltas(&mut br, flat, &mut state.props, prop_proto4, separated)?;
                }
                world.entities.insert(eid, state);
            }
            // 00 = Delta - update existing entity. A delta can legitimately
            // reference an entity we never saw ENTER (the server deltas against a
            // baseline frame the demo never carried - common for high-index sandbox
            // props in GMod); we can't know its class to size the field list, so the
            // packet can't continue. Low-index entities (players) decode first, so
            // their scrape still lands - see the caller, which scrapes regardless.
            0b00 => {
                let cid = world.entities.get(&eid).map(|s| s.class_id)?;
                let flat = data.flat_props.get(&cid)?;
                let state = world.entities.get_mut(&eid).unwrap();
                read_prop_deltas(&mut br, flat, &mut state.props, prop_proto4, separated)?;
            }
            // 01 = Leave PVS - no further bits
            0b01 => { /* keep entity state, just out of PVS */ }
            // 11 = Delete - no further bits, but mark removed
            0b11 => {
                world.entities.remove(&eid);
            }
            _ => unreachable!(),
        }
    }

    // Removed-entities list (only when has_delta_tick is set)
    if has_delta_tick {
        while br.bit_pos() < end_bit {
            let more = br.read_bool()?;
            if !more {
                break;
            }
            let removed = br.read_bits(edict_bits)? as u16;
            world.entities.remove(&removed);
        }
    }

    Some(())
}

/// Read a series of (prop-index delta + value) updates, terminated by a 0
/// stop-bit. Each update: 1-bit has-more, then bit_var index delta, then
/// SendProp value.
fn read_prop_deltas(
    br: &mut BitReader,
    flat: &[SendPropDef],
    out: &mut HashMap<usize, PropValue>,
    proto4: bool,
    separated: bool,
) -> Option<()> {
    let mut idx: i32 = -1;
    if separated {
        // CS:GO (proto-4 container, but newer entity encoding): the field-index
        // list and the value list are *separated*, not interleaved. The engine
        // reads every changed prop index first (terminated by 0xFFF), then reads
        // all the values in that order. (Verified against demoinfocs-golang
        // `Entity.ApplyUpdate`.) The Portal 2 / Source-2009 path below interleaves
        // index+value per prop, so this is a distinct mode.
        let new_way = br.read_bool()?;
        let mut indices: Vec<usize> = Vec::new();
        loop {
            let next = read_field_index(br, idx, new_way)?;
            if next == -1 {
                break;
            }
            idx = next;
            if idx < 0 || idx as usize >= flat.len() {
                return None;
            }
            indices.push(idx as usize);
        }
        for i in indices {
            let val = decode_prop(&flat[i], br)?;
            out.insert(i, val);
        }
    } else if proto4 {
        // Proto-4 (Portal 2 / Source 2009): CDeltaBitsReader. The constructor
        // pre-reads ONE bit = `new_way`, used for every field-index read in
        // this entity. (Confirmed via engine.dll CDeltaBitsReader in IDA - the
        // ctor reads a bit into the struct, ReadNextPropIndex branches on it.)
        // Missing this leading bit is what desynced every earlier attempt.
        let new_way = br.read_bool()?;
        loop {
            let next = read_field_index(br, idx, new_way)?;
            if next == -1 {
                break;
            }
            idx = next;
            if idx < 0 || idx as usize >= flat.len() {
                return None;
            }
            let prop = &flat[idx as usize];
            let val = decode_prop(prop, br)?;
            out.insert(idx as usize, val);
        }
    } else {
        loop {
            let more = br.read_bool()?;
            if !more {
                break;
            }
            let diff = read_bit_var(br)? as i32;
            idx = idx.saturating_add(diff).saturating_add(1);
            if idx < 0 || idx as usize >= flat.len() {
                return None;
            }
            let prop = &flat[idx as usize];
            let val = decode_prop(prop, br)?;
            out.insert(idx as usize, val);
        }
    }
    Some(())
}

// Proto-4 field-index reader (CDeltaBitsReader::ReadNextPropIndex). Encoding
// derived from engine.dll disassembly:
//   if new_way && read_bit: return last + 1           (consecutive)
//   if new_way && read_bit: ret = read_bits(3)        (small gap)
//   else: ret = read_bits(7); then by ret & 0x60:
//       0x20 -> ret = (ret & ~0x60) | (read_bits(2) << 5)
//       0x40 -> ret = (ret & ~0x60) | (read_bits(4) << 5)
//       0x60 -> ret = (ret & ~0x60) | (read_bits(7) << 5)
//   if ret == 0xFFF: end (return -1)
//   return last + ret + 1
fn read_field_index(br: &mut BitReader, last: i32, new_way: bool) -> Option<i32> {
    if new_way && br.read_bool()? {
        return Some(last + 1);
    }
    let ret = if new_way && br.read_bool()? {
        br.read_bits(3)? as i32
    } else {
        let mut r = br.read_bits(7)? as i32;
        match r & 0x60 {
            0x20 => r = (r & !0x60) | ((br.read_bits(2)? as i32) << 5),
            0x40 => r = (r & !0x60) | ((br.read_bits(4)? as i32) << 5),
            0x60 => r = (r & !0x60) | ((br.read_bits(7)? as i32) << 5),
            _ => {}
        }
        r
    };
    if ret == 0xFFF {
        return Some(-1);
    }
    Some(last + ret + 1)
}
