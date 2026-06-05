// String tables — only what's needed for entity baselines.
//
// Ported from dotabuff/manta `string_table.go`. We track tables by id/name and
// parse their bit-packed entry lists. The one table that matters for positions
// is `instancebaseline`: key = class id (as ascii), value = the baseline blob a
// freshly-spawned entity of that class is initialised from before its delta.

use super::bitreader::BitReader;
use super::snappy;

const KEY_HISTORY_SIZE: usize = 32;

pub struct StringTable {
    pub name: String,
    pub user_data_fixed_size: bool,
    pub user_data_size_bits: i32,
    pub flags: i32,
    pub varint_bitcounts: bool,
    pub items: std::collections::HashMap<i32, (String, Vec<u8>)>, // index -> (key, value)
}

/// One parsed entry: (index, key, value).
pub struct Item {
    pub index: i32,
    pub key: String,
    pub value: Vec<u8>,
}

/// Decode a CreateStringTable string_data blob (already decompressed if needed).
pub fn parse_entries(
    buf: &[u8],
    num_updates: i32,
    user_data_fixed: bool,
    user_data_size_bits: i32,
    flags: i32,
    varint_bitcounts: bool,
) -> Vec<Item> {
    let mut items = Vec::new();
    if buf.is_empty() {
        return items;
    }
    let mut r = BitReader::new(buf);
    let mut index: i32 = -1;
    let mut keys: Vec<String> = Vec::with_capacity(KEY_HISTORY_SIZE);

    for _ in 0..num_updates {
        let mut key = String::new();
        let mut value: Vec<u8> = Vec::new();

        // Index: increment, or absolute+1.
        if r.read_bit() {
            index += 1;
        } else {
            index = r.read_var_u32() as i32 + 1;
        }

        // Optional key, possibly built from the key-history ring.
        let has_key = r.read_bit();
        if has_key {
            let use_history = r.read_bit();
            if use_history {
                let pos = r.read_bits(5) as usize;
                let size = r.read_bits(5) as usize;
                if pos >= keys.len() {
                    key.push_str(&r.read_string());
                } else {
                    let s = &keys[pos];
                    if size > s.len() {
                        key.push_str(s);
                        key.push_str(&r.read_string());
                    } else {
                        key.push_str(&s[..size]);
                        key.push_str(&r.read_string());
                    }
                }
            } else {
                key = r.read_string();
            }

            if keys.len() >= KEY_HISTORY_SIZE {
                keys.remove(0);
            }
            keys.push(key.clone());
        }

        // Optional value.
        let has_value = r.read_bit();
        if has_value {
            let bit_size: u32;
            let mut is_compressed = false;
            if user_data_fixed {
                bit_size = user_data_size_bits as u32;
            } else {
                if flags & 0x1 != 0 {
                    is_compressed = r.read_bit();
                }
                if varint_bitcounts {
                    bit_size = r.read_ubit_var() * 8;
                } else {
                    bit_size = r.read_bits(17) * 8;
                }
            }
            value = r.read_bits_as_bytes(bit_size);
            if is_compressed {
                if let Some(d) = snappy::decompress(&value) {
                    value = d;
                }
            }
        }

        items.push(Item { index, key, value });
    }

    items
}

/// Decompress a CreateStringTable string_data blob per its compressed flag.
/// Returns the (possibly unchanged) bytes; LZSS (old replays) is unsupported and
/// returns None so the caller skips the table.
pub fn maybe_decompress(buf: &[u8], compressed: bool) -> Option<Vec<u8>> {
    if !compressed {
        return Some(buf.to_vec());
    }
    if buf.len() >= 4 && &buf[..4] == b"LZSS" {
        return None; // old-replay LZSS not supported (CS2 uses snappy)
    }
    snappy::decompress(buf)
}
