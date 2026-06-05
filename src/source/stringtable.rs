// DEM_STRINGTABLES userinfo extractor.
//
// The userinfo string table maps each player slot to its player_info_t blob,
// from which we read name, steam_id, user_id, and the fakeplayer flag.
//
// This is essentially the same routine that lives in main.rs (legacy POV
// userinfo parser), lifted into the new module for reuse and to keep the
// `source_demo` namespace self-contained.

use super::bitreader::BitReader;

#[derive(Debug, Clone, Default)]
pub struct PlayerInfo {
    pub name: String,
    pub user_id: u32,
    pub steam_id: String,
    pub is_fake: bool,
    pub is_hltv: bool,
    /// Every distinct name this slot has had (signon name first, then any
    /// mid-demo rename / disconnect-reconnect targets). Used for matching
    /// the recorder's header nick against players who later renamed.
    pub aliases: Vec<String>,
}

/// Result of parsing the DEM_STRINGTABLES payload.
pub struct StringTablesParse {
    pub players: std::collections::HashMap<u32, PlayerInfo>,
    /// Ordered list of table names - the index in this list is the table_id
    /// the server uses when sending svc_UpdateStringTable.
    pub table_names: Vec<String>,
}

/// Returns the parsed userinfo entries plus the ordered table-name list.
/// Source's userinfo strings are keyed by slot (0..num_strings); we report
/// entity_id = slot + 1, matching the engine convention.
pub fn parse_userinfo(payload: &[u8]) -> Option<StringTablesParse> {
    let mut br = BitReader::new(payload);
    let num_tables = br.read_bits(8)? as usize;
    if num_tables == 0 || num_tables > 64 { return None; }
    let mut out = std::collections::HashMap::new();
    let mut table_names = Vec::with_capacity(num_tables);
    for _ in 0..num_tables {
        let table_name = br.read_cstring(128)?;
        table_names.push(table_name.clone());
        let num_strings = br.read_bits(16)? as usize;
        // u16 max = 65535; real tables (soundprecache, modelprecache) can have
        // thousands of entries. No sanity cap needed beyond u16.
        let is_userinfo = table_name == "userinfo";
        for slot in 0..num_strings {
            let _player_name = br.read_cstring(512)?;
            let has_ud = br.read_bool()?;
            if has_ud {
                let ud_len = br.read_bits(16)? as usize;
                if ud_len > 65535 { return None; }
                if is_userinfo && ud_len >= 36 {
                    let mut bytes = vec![0u8; ud_len];
                    for i in 0..ud_len { bytes[i] = br.read_byte()?; }
                    if let Some(pi) = parse_player_info_blob(&bytes) {
                        let entity_id = (slot as u32) + 1;
                        out.insert(entity_id, pi);
                    }
                    continue;
                } else {
                    for _ in 0..ud_len { br.read_byte()?; }
                }
            }
            // No fallback for entries without player_info_t - tf-demo-parser
            // also skips slots whose steam_id is empty (= no real player).
            // The bare entry text is just a slot-number string, not a name.
        }
        // Client-side data section
        let has_cs = br.read_bool()?;
        if has_cs {
            let n = br.read_bits(16)? as usize;
            for _ in 0..n {
                br.read_cstring(512)?;
                let hud = br.read_bool()?;
                if hud {
                    let l = br.read_bits(16)? as usize;
                    for _ in 0..l { br.read_byte()?; }
                }
            }
        }
    }
    if out.is_empty() && table_names.is_empty() { None }
    else { Some(StringTablesParse { players: out, table_names }) }
}

/// Parse a `player_info_s` userdata blob. The struct's layout differs by engine
/// generation in three ways: the leading prefix, the name-field width, and the
/// integer byte order:
///
///   TF2 / CS:S / HL2 (proto-3):     no prefix,        name[32] at offset 0,  LE
///   Portal 2 / Stanley / L4D(2):    xuid(8) prefix,   name[32] at offset 8,  LE
///   CS:GO:                          version(8)+xuid(8) prefix (16),
///                                   name[128] at offset 16,                  BE
///
/// CS:GO's blob is ~344 bytes (the 128-byte name); the older variants are ≤144.
/// We use that size split to pick the name width and prefix candidates, then
/// from `name_off` the relative layout is:
///   name_off+0 ..+N   : name[N] (null-terminated, N = 32 or 128)
///   name_off+N ..+N+4 : user_id (u32, BE on CS:GO else LE)
///   name_off+N+4..    : guid / steam_id (null-terminated, "STEAM_…" or "[U:…]")
///   name_off+108      : is_fake_player (u8)  (best-effort, name[32] layout only)
///   name_off+109      : is_hl_tv (u8)
///
/// We auto-detect `name_off` rather than thread a per-game flag: offset 0 holds
/// a printable name on proto-3; on proto-4 it holds raw xuid bytes (non-ASCII)
/// and the printable name sits at offset 8. This stays correct across the whole
/// proto-4 family without needing to know the exact game.
pub fn parse_player_info_blob(bytes: &[u8]) -> Option<PlayerInfo> {
    if std::env::var("DUMP_USERINFO").is_ok() {
        let ascii: String = bytes.iter().take(120).map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
        eprintln!("[UINFO] len={} ascii=\"{}\"", bytes.len(), ascii);
    }
    let printable = |b: u8| b.is_ascii_graphic() || b == b' ';
    // CS:GO carries a 16-byte version+xuid prefix and a 128-byte name, making the
    // blob ~344B; older Source uses a 0/8-byte prefix and a 32-byte name (≤144B).
    let csgo = bytes.len() >= 200;
    let name_len = if csgo { 128 } else { 32 };
    // Find the prefix by locating the first candidate offset that begins a
    // printable name. Bias toward 16 for the large CS:GO blob, where offset 8 can
    // land mid-xuid on a stray printable byte.
    let candidates: &[usize] = if csgo { &[16, 8, 0] } else { &[0, 8, 16] };
    let name_off = candidates.iter().copied()
        .find(|&off| bytes.len() >= off + name_len + 4 && bytes.get(off).is_some_and(|&b| printable(b)))
        .unwrap_or(0);
    let at = |o: usize| name_off + o;
    if bytes.len() < at(name_len) + 4 { return None; }
    // user_id sits right after the name field; CS:GO writes integers big-endian.
    let uid_bytes = [bytes[at(name_len)], bytes[at(name_len) + 1], bytes[at(name_len) + 2], bytes[at(name_len) + 3]];
    let uid = if csgo { u32::from_be_bytes(uid_bytes) } else { u32::from_le_bytes(uid_bytes) };
    // is_fake / is_hltv offsets are only mapped for the name[32] layout.
    let is_fake = !csgo && bytes.len() > at(108) && bytes[at(108)] != 0;
    let is_hltv = !csgo && bytes.len() > at(109) && bytes[at(109)] != 0;
    let name_end = bytes[at(0)..at(name_len)].iter().position(|&b| b == 0).unwrap_or(name_len);
    let actual_name = String::from_utf8_lossy(&bytes[at(0)..at(0) + name_end]).into_owned();
    if actual_name.is_empty() { return None; }
    // guid / steam_id is the null-terminated string right after user_id.
    let guid_off = at(name_len) + 4;
    let steam_id: String = bytes[guid_off..(guid_off + 33).min(bytes.len())]
        .iter().take_while(|&&b| b != 0)
        .map(|&b| b as char).collect();
    Some(PlayerInfo {
        aliases: vec![actual_name.clone()],
        name: actual_name, user_id: uid, steam_id, is_fake, is_hltv,
    })
}
