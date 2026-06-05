// Entity state + field reading.
//
// Ported from dotabuff/manta `field_state.go`, `field_reader.go`, `entity.go`.
// An entity holds a sparse tree of decoded values addressed by field paths. To
// apply a delta we read the field paths (Huffman), resolve each to its decoder,
// decode the value, and set it into the tree.

use super::bitreader::BitReader;
use super::fieldpath::{read_field_paths, FieldPath};
use super::serializer::{decode_value, FieldValue, Tables};

enum Slot {
    Empty,
    Value(FieldValue),
    Sub(FieldState),
}

pub struct FieldState {
    slots: Vec<Slot>,
}

impl FieldState {
    pub fn new() -> Self {
        FieldState { slots: Vec::with_capacity(8) }
    }

    fn ensure(&mut self, z: usize) {
        if self.slots.len() <= z {
            let want = (z + 1).max(self.slots.len() * 2).max(8);
            while self.slots.len() < want {
                self.slots.push(Slot::Empty);
            }
        }
    }

    pub fn set(&mut self, fp: &FieldPath, v: FieldValue) {
        let mut cur = self;
        for i in 0..=fp.last {
            let z = fp.path[i] as usize;
            cur.ensure(z);
            if i == fp.last {
                if !matches!(cur.slots[z], Slot::Sub(_)) {
                    cur.slots[z] = Slot::Value(v);
                }
                return;
            }
            if !matches!(cur.slots[z], Slot::Sub(_)) {
                cur.slots[z] = Slot::Sub(FieldState::new());
            }
            cur = match &mut cur.slots[z] {
                Slot::Sub(s) => s,
                _ => return,
            };
        }
    }

    pub fn get(&self, fp: &FieldPath) -> Option<&FieldValue> {
        let mut cur = self;
        for i in 0..=fp.last {
            let z = fp.path[i] as usize;
            if z >= cur.slots.len() {
                return None;
            }
            if i == fp.last {
                return match &cur.slots[z] {
                    Slot::Value(v) => Some(v),
                    _ => None,
                };
            }
            cur = match &cur.slots[z] {
                Slot::Sub(s) => s,
                _ => return None,
            };
        }
        None
    }
}

pub struct Entity {
    pub index: i32,
    pub serial: i32,
    pub class_id: i32,
    pub serializer_idx: usize,
    pub state: FieldState,
    pub active: bool,
}

/// Apply a delta from `r` to `state` using `ser_idx`'s serializer. Returns false
/// on an unresolvable field path (a desync) so the caller can stop this packet.
pub fn read_fields(tables: &Tables, ser_idx: usize, r: &mut BitReader, state: &mut FieldState) -> bool {
    let paths = match read_field_paths(r) {
        Some(p) => p,
        None => return false, // desync — fail the packet
    };
    for fp in &paths {
        match tables.decoder_for_path(ser_idx, fp, 0) {
            Some(d) => {
                let v = decode_value(d, r);
                state.set(fp, v);
            }
            None => return false,
        }
    }
    true
}

/// Read a value off an entity by dotted field name (e.g. "CBodyComponent.m_cellX").
pub fn get_by_name<'a>(tables: &Tables, e: &'a Entity, name: &str) -> Option<&'a FieldValue> {
    let fp = tables.path_for_name(e.serializer_idx, name)?;
    e.state.get(&fp)
}
