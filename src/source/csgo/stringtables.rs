// CS:GO protobuf string-table decode.
//
// Where older Source ships svc_CreateStringTable / svc_UpdateStringTable as
// hand-rolled bit-packed messages (decoded inline in `player_tracks::scan`),
// CS:GO wraps the *envelope* in protobuf — but the entry payload inside the
// `string_data` field is still the same engine bit-stream. So this module:
//
//   1. parses the protobuf envelope (CSVCMsg_CreateStringTable /
//      CSVCMsg_UpdateStringTable, ids 12 / 13), and
//   2. walks the bit-packed entry stream in `string_data` exactly as the engine's
//      `CNetworkStringTable::ParseUpdate` does.
//
// We only act on the `userinfo` table — its per-entry userdata is the
// `player_info_s` blob that carries names / steam_ids / user_ids. Every other
// table is still recorded (name + params) so that an UpdateStringTable's numeric
// `table_id` — which is just the table's creation order — resolves correctly.
//
// This is what lifts CS:GO from "names as of the DEM_STRINGTABLES snapshot" to
// live mid-demo updates: late joiners, reconnects, and in-game renames.

use std::collections::HashMap;

use super::super::bitreader::BitReader;
use super::super::stringtable::{parse_player_info_blob, PlayerInfo};
use super::super::super::protobuf::Reader;

/// SUBSTRING_BITS in the engine — width of the "bytes to copy from history"
/// field when an entry string is delta-coded against an earlier one.
const SUBSTRING_BITS: u32 = 5;
/// MAX_USERDATA_BITS — width of the per-entry userdata byte count when the table
/// is *not* fixed-size.
const MAX_USERDATA_BITS: u32 = 14;

/// Metadata for one created string table, indexed by creation order (= the
/// `table_id` that UpdateStringTable references).
struct TableMeta {
    name: String,
    /// Bits to encode an absolute entry index = log2(max_entries).
    entry_bits: u32,
    user_data_fixed_size: bool,
    user_data_size_bits: u32,
}

/// Per-demo string-table state, threaded through the CS:GO packet scan. Holds
/// the table registry so UpdateStringTable diffs can be applied to the right
/// table across packets.
#[derive(Default)]
pub struct StringTables {
    tables: Vec<TableMeta>,
}

impl StringTables {
    pub fn new() -> Self {
        StringTables::default()
    }

    /// Handle a `svc_CreateStringTable` (id 12) body. Registers the table and, if
    /// it is `userinfo`, decodes its initial entries into `names`.
    pub fn handle_create(&mut self, body: &[u8], names: &mut HashMap<u32, PlayerInfo>) {
        let Some(c) = parse_create(body) else { return };
        let entry_bits = log2(c.max_entries);
        let is_userinfo = c.name == "userinfo";
        self.tables.push(TableMeta {
            name: c.name,
            entry_bits,
            user_data_fixed_size: c.user_data_fixed_size,
            user_data_size_bits: c.user_data_size_bits,
        });
        if is_userinfo {
            decode_userinfo_entries(
                c.string_data, c.num_entries, entry_bits,
                c.user_data_fixed_size, c.user_data_size_bits, names,
            );
        }
    }

    /// Handle a `svc_UpdateStringTable` (id 13) body. Applies the diff to the
    /// table named by `table_id`; only `userinfo` changes touch `names`.
    pub fn handle_update(&mut self, body: &[u8], names: &mut HashMap<u32, PlayerInfo>) {
        let Some(u) = parse_update(body) else { return };
        let Some(meta) = self.tables.get(u.table_id as usize) else { return };
        if meta.name != "userinfo" {
            return;
        }
        decode_userinfo_entries(
            u.string_data, u.num_changed_entries, meta.entry_bits,
            meta.user_data_fixed_size, meta.user_data_size_bits, names,
        );
    }
}

/// Parsed `CSVCMsg_CreateStringTable` envelope (only the fields we use).
struct Create<'a> {
    name: String,
    max_entries: u32,
    num_entries: u32,
    user_data_fixed_size: bool,
    user_data_size_bits: u32,
    string_data: &'a [u8],
}

fn parse_create(body: &[u8]) -> Option<Create<'_>> {
    let mut r = Reader::new(body);
    let mut name = String::new();
    let mut max_entries = 0u32;
    let mut num_entries = 0u32;
    let mut user_data_fixed_size = false;
    let mut user_data_size_bits = 0u32;
    let mut string_data: &[u8] = &[];
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            1 => name = f.value.as_str()?.into_owned(),
            2 => max_entries = f.value.as_u32()?,
            3 => num_entries = f.value.as_u32()?,
            4 => user_data_fixed_size = f.value.as_bool()?,
            6 => user_data_size_bits = f.value.as_u32()?,
            8 => string_data = f.value.as_bytes()?,
            _ => {}
        }
    }
    if max_entries == 0 {
        return None;
    }
    Some(Create { name, max_entries, num_entries, user_data_fixed_size, user_data_size_bits, string_data })
}

/// Parsed `CSVCMsg_UpdateStringTable` envelope.
struct Update<'a> {
    table_id: u32,
    num_changed_entries: u32,
    string_data: &'a [u8],
}

fn parse_update(body: &[u8]) -> Option<Update<'_>> {
    let mut r = Reader::new(body);
    let mut table_id = 0u32;
    // num_changed_entries defaults to 1 when omitted (engine default).
    let mut num_changed_entries = 1u32;
    let mut string_data: &[u8] = &[];
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            1 => table_id = f.value.as_u32()?,
            2 => num_changed_entries = f.value.as_u32()?,
            3 => string_data = f.value.as_bytes()?,
            _ => {}
        }
    }
    Some(Update { table_id, num_changed_entries, string_data })
}

/// Walk the bit-packed entry stream of the userinfo table and merge each changed
/// entry's `player_info_s` blob into `names`, keyed by entity_id = slot + 1.
///
/// Mirrors `CNetworkStringTable::ParseUpdate`: per entry, an optional absolute
/// index (else last+1), an optional (possibly history-delta-coded) name string,
/// then optional userdata — fixed-width or a 14-bit-counted byte run. We don't
/// reconstruct the name string (the userinfo entry name is just the slot index);
/// we only advance the cursor past it correctly to reach the userdata blob.
fn decode_userinfo_entries(
    string_data: &[u8],
    num_entries: u32,
    entry_bits: u32,
    user_data_fixed_size: bool,
    user_data_size_bits: u32,
    names: &mut HashMap<u32, PlayerInfo>,
) {
    let mut br = BitReader::new(string_data);

    // Leading flag: the stream may be dictionary-encoded (a CS:GO addition). That
    // variant isn't used for userinfo and isn't implemented (Valve's demoinfogo
    // bails on it too), so if it's set we can't safely walk the entries.
    match br.read_bool() {
        Some(false) => {}
        Some(true) => return,
        None => return,
    }

    let mut last_entry: i32 = -1;

    for _ in 0..num_entries {
        // Entry index: a 1 bit means "previous + 1", else an absolute index.
        let entry: i32 = match br.read_bool() {
            Some(true) => last_entry + 1,
            Some(false) => match br.read_bits(entry_bits) {
                Some(v) => v as i32,
                None => return,
            },
            None => return,
        };
        last_entry = entry;

        // Optional entry string (delta-coded against a 32-deep history). We only
        // need to skip it; the history index (5) + bytes-to-copy (5) prefix and
        // the null-terminated suffix together advance the cursor correctly.
        match br.read_bool() {
            Some(true) => {
                let is_substring = match br.read_bool() {
                    Some(b) => b,
                    None => return,
                };
                if is_substring && !br.skip(5 + SUBSTRING_BITS) {
                    return;
                }
                if br.read_cstring(1024).is_none() {
                    return;
                }
            }
            Some(false) => {}
            None => return,
        }

        // Optional userdata: fixed-width blob, or a 14-bit-counted byte run.
        match br.read_bool() {
            Some(true) => {
                let bytes = if user_data_fixed_size {
                    match read_bits_as_bytes(&mut br, user_data_size_bits) {
                        Some(b) => b,
                        None => return,
                    }
                } else {
                    let nbytes = match br.read_bits(MAX_USERDATA_BITS) {
                        Some(v) => v as usize,
                        None => return,
                    };
                    match read_bits_as_bytes(&mut br, nbytes as u32 * 8) {
                        Some(b) => b,
                        None => return,
                    }
                };
                if let Some(mut pi) = parse_player_info_blob(&bytes) {
                    let entity_id = (entry as u32) + 1;
                    // Preserve every prior alias for this slot, then add the new
                    // name (matching the bit-packed userinfo-update path).
                    if let Some(prev) = names.get(&entity_id) {
                        pi.aliases = prev.aliases.clone();
                    }
                    if !pi.aliases.iter().any(|a| a == &pi.name) {
                        pi.aliases.push(pi.name.clone());
                    }
                    names.insert(entity_id, pi);
                }
            }
            Some(false) => {}
            None => return,
        }
    }
}

/// Read `n_bits` from `br` and pack them into bytes (8 bits per byte, low bits
/// first — the engine's `ReadBits` byte order). A trailing partial byte keeps
/// whatever bits remain. Returns `None` if the stream runs short.
fn read_bits_as_bytes(br: &mut BitReader, n_bits: u32) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((n_bits as usize).div_ceil(8));
    let mut remaining = n_bits;
    while remaining >= 8 {
        out.push(br.read_bits(8)? as u8);
        remaining -= 8;
    }
    if remaining > 0 {
        out.push(br.read_bits(remaining)? as u8);
    }
    Some(out)
}

/// floor(log2(n)) for n >= 1 — the engine's entry-index bit width for a table of
/// `n` max entries (always a power of two).
fn log2(n: u32) -> u32 {
    if n <= 1 {
        0
    } else {
        31 - n.leading_zeros()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LSB-first bit writer matching `BitReader`'s order: bit `i` of a value lands
    /// at successive stream positions, low bit first.
    struct BitWriter {
        bits: Vec<bool>,
    }
    impl BitWriter {
        fn new() -> Self {
            BitWriter { bits: Vec::new() }
        }
        fn put(&mut self, value: u32, n: u32) {
            for i in 0..n {
                self.bits.push((value >> i) & 1 == 1);
            }
        }
        fn put_bytes(&mut self, bytes: &[u8]) {
            for &b in bytes {
                self.put(b as u32, 8);
            }
        }
        fn into_bytes(self) -> Vec<u8> {
            let mut out = vec![0u8; self.bits.len().div_ceil(8)];
            for (i, &bit) in self.bits.iter().enumerate() {
                if bit {
                    out[i / 8] |= 1 << (i % 8);
                }
            }
            out
        }
    }

    /// A 344-byte CS:GO `player_info_s` blob (16-byte version+xuid prefix, name at
    /// offset 16, big-endian user_id after name[128], steam_id after that) — the
    /// layout `parse_player_info_blob` auto-detects for CS:GO.
    fn csgo_player_blob(name: &str, uid: u32, steam: &str) -> Vec<u8> {
        let mut b = vec![0u8; 344];
        b[16..16 + name.len()].copy_from_slice(name.as_bytes());
        b[144..148].copy_from_slice(&uid.to_be_bytes());
        b[148..148 + steam.len()].copy_from_slice(steam.as_bytes());
        b
    }

    /// Append a protobuf field: a Len (wire 2) field carrying `data`.
    fn push_len_field(buf: &mut Vec<u8>, field: u32, data: &[u8]) {
        buf.push(((field << 3) | 2) as u8);
        // length as a single-byte varint suffices for our test sizes < 128, but
        // string_data is 344+ bytes, so emit a real varint.
        let mut len = data.len();
        loop {
            let mut byte = (len & 0x7f) as u8;
            len >>= 7;
            if len != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if len == 0 {
                break;
            }
        }
        buf.extend_from_slice(data);
    }

    /// Append a protobuf varint field (wire 0).
    fn push_varint_field(buf: &mut Vec<u8>, field: u32, mut value: u64) {
        buf.push(((field << 3) | 0) as u8);
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    /// Build the bit-packed `string_data` for one userinfo entry at index 0 whose
    /// userdata is `blob` (variable-size, 14-bit byte count, no entry string).
    fn one_entry_string_data(blob: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.put(0, 1); // not dictionary-encoded
        w.put(1, 1); // "next entry" → index 0 (last + 1)
        w.put(0, 1); // no entry string
        w.put(1, 1); // has userdata
        w.put(blob.len() as u32, MAX_USERDATA_BITS); // byte count
        w.put_bytes(blob);
        w.into_bytes()
    }

    #[test]
    fn create_userinfo_decodes_player() {
        let blob = csgo_player_blob("Sierra", 7, "STEAM_1:0:42");
        let string_data = one_entry_string_data(&blob);

        let mut body = Vec::new();
        push_len_field(&mut body, 1, b"userinfo"); // name
        push_varint_field(&mut body, 2, 256); // max_entries
        push_varint_field(&mut body, 3, 1); // num_entries
        push_len_field(&mut body, 8, &string_data); // string_data

        let mut tables = StringTables::new();
        let mut names = HashMap::new();
        tables.handle_create(&body, &mut names);

        // entity_id = slot 0 + 1.
        let pi = names.get(&1).expect("entry decoded");
        assert_eq!(pi.name, "Sierra");
        assert_eq!(pi.user_id, 7);
        assert_eq!(pi.steam_id, "STEAM_1:0:42");
    }

    #[test]
    fn update_targets_table_by_creation_order() {
        // Create two tables so userinfo lands at table_id 1, then update it.
        let mut tables = StringTables::new();
        let mut names = HashMap::new();

        let mut other = Vec::new();
        push_len_field(&mut other, 1, b"downloadables");
        push_varint_field(&mut other, 2, 8192);
        push_varint_field(&mut other, 3, 0);
        push_len_field(&mut other, 8, &[0x00]); // empty (dictionary bit = 0, no entries)
        tables.handle_create(&other, &mut names);

        let mut create_ui = Vec::new();
        push_len_field(&mut create_ui, 1, b"userinfo");
        push_varint_field(&mut create_ui, 2, 256);
        push_varint_field(&mut create_ui, 3, 0);
        push_len_field(&mut create_ui, 8, &[0x00]);
        tables.handle_create(&create_ui, &mut names);

        // Now an UpdateStringTable for table_id 1 (userinfo) with one entry.
        let blob = csgo_player_blob("Latecomer", 99, "STEAM_1:1:1");
        let string_data = one_entry_string_data(&blob);
        let mut update = Vec::new();
        push_varint_field(&mut update, 1, 1); // table_id
        push_varint_field(&mut update, 2, 1); // num_changed_entries
        push_len_field(&mut update, 3, &string_data); // string_data
        tables.handle_update(&update, &mut names);

        let pi = names.get(&1).expect("update decoded");
        assert_eq!(pi.name, "Latecomer");
        assert_eq!(pi.user_id, 99);

        // An update to a non-userinfo table_id must not touch names.
        let mut wrong = Vec::new();
        push_varint_field(&mut wrong, 1, 0); // downloadables
        push_varint_field(&mut wrong, 2, 1);
        push_len_field(&mut wrong, 3, &one_entry_string_data(&csgo_player_blob("Ghost", 1, "x")));
        tables.handle_update(&wrong, &mut names);
        assert_eq!(names.get(&1).unwrap().name, "Latecomer"); // unchanged
    }

    #[test]
    fn dictionary_encoded_stream_bails_safely() {
        // Leading dictionary bit set → we can't walk it; must not panic or
        // misdecode. string_data = single byte with bit 0 set.
        let mut names = HashMap::new();
        decode_userinfo_entries(&[0x01], 5, 8, false, 0, &mut names);
        assert!(names.is_empty());
    }
}
