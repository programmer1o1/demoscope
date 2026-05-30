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

/// Parse a `player_info_s` userdata blob. The struct's leading layout differs
/// by engine generation:
///
///   TF2 / CS:S / HL2 (proto-3):     name[32] starts at offset 0.
///   Portal 2 / Stanley (proto-4):   an 8-byte `xuid` (uint64) precedes name,
///                                    so name[32] starts at offset 8.
///
/// From `name_off` onward the relative layout is identical:
///   name_off+0 ..+32  : name[32] (null-terminated)
///   name_off+32..+36  : user_id (u32)
///   name_off+36..     : guid / steam_id (null-terminated, "STEAM_…" or "[U:…]")
///   name_off+108      : is_fake_player (u8)  (best-effort)
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
    let name_off = if bytes.first().is_some_and(|&b| printable(b)) {
        0
    } else if bytes.len() >= 8 + 36 && bytes.get(8).is_some_and(|&b| printable(b)) {
        8 // proto-4 xuid prefix
    } else {
        0
    };
    if bytes.len() < name_off + 36 { return None; }
    let at = |o: usize| name_off + o;
    let uid = u32::from_le_bytes([bytes[at(32)], bytes[at(33)], bytes[at(34)], bytes[at(35)]]);
    let is_fake = bytes.len() > at(108) && bytes[at(108)] != 0;
    let is_hltv = bytes.len() > at(109) && bytes[at(109)] != 0;
    let name_end = bytes[at(0)..at(32)].iter().position(|&b| b == 0).unwrap_or(32);
    let actual_name = String::from_utf8_lossy(&bytes[at(0)..at(0) + name_end]).into_owned();
    if actual_name.is_empty() { return None; }
    let steam_id: String = bytes[at(36)..at(36) + 32.min(bytes.len().saturating_sub(at(36)))]
        .iter().take_while(|&&b| b != 0)
        .map(|&b| b as char).collect();
    Some(PlayerInfo {
        aliases: vec![actual_name.clone()],
        name: actual_name, user_id: uid, steam_id, is_fake, is_hltv,
    })
}
