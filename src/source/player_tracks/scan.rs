// Demo packet / svc-message scanning for player tracking. Walks each PACKET
// frame's payload — both the CS:GO protobuf path (`scan_csgo_payload`) and the
// pre-CS:GO bitstream path (`scan_game_payload`) — and scrapes per-entity
// position/life/weapon state into the caller's `OriginTracker` map. The demo
// container walk and result assembly live in the parent `player_tracks` module,
// which owns the `le_*`/`read_cstring` readers and the DEM_* command constants.

use std::collections::HashMap;

use super::super::bitreader::BitReader;
use super::super::csgo;
use super::super::datatable::DataTables;
use super::super::packetentities::{parse_entity_updates, EntityWorld};
use super::super::stringtable::PlayerInfo;
use super::{read_cstring, PlayerEcon, DEM_STRINGTABLES};

// Scan the game-packet payload for svc_PacketEntities (type 26). Every other
// known message type is skipped using its length field; unknown types abort
// the scan since we can't advance the bit cursor past them.
/// CS:GO game-packet payload: a sequence of protobuf-framed net messages. We
/// decode `svc_PacketEntities` into the entity world, then scrape per-player
/// position / angle / life tracks the same way the bit-packed path does. The
/// recorder camera comes from the democmdinfo viewAngles (captured by the
/// caller), not from a net message, so it isn't handled here. Player names start
/// from the `DEM_STRINGTABLES` userinfo snapshot (shared with the bit-packed
/// path) and are then kept live by decoding the protobuf svc_*StringTable
/// messages here (see `csgo::stringtables`) — catching renames, late joiners,
/// and reconnects that happen after the snapshot.
#[allow(clippy::too_many_arguments)]
pub(super) fn scan_csgo_payload(
    payload: &[u8],
    tick: i32,
    data: Option<&DataTables>,
    world: Option<&mut EntityWorld>,
    last_pos: &mut HashMap<u32, (f32, f32, f32)>,
    origin_state: &mut HashMap<u32, OriginTracker>,
    last_life: &mut HashMap<u32, u8>,
    last_obs: &mut HashMap<u32, u8>,
    last_yaw: &mut HashMap<u32, (f32, f32)>,
    last_weapon: &mut HashMap<u32, i32>,
    tracks: &mut HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    life_states: &mut HashMap<u32, Vec<(i32, u8)>>,
    observer_modes: &mut HashMap<u32, Vec<(i32, u8)>>,
    yaws: &mut HashMap<u32, Vec<(i32, f32, f32)>>,
    weapons: &mut HashMap<u32, Vec<(i32, i32)>>,
    weapon_classes: &mut HashMap<i32, String>,
    econ: &mut HashMap<u32, PlayerEcon>,
    // Persistent string-table registry: tracks created tables so mid-demo
    // svc_UpdateStringTable diffs (renames, late joiners, reconnects) on the
    // userinfo table merge into `names`.
    string_tables: &mut csgo::stringtables::StringTables,
    names: &mut HashMap<u32, PlayerInfo>,
) {
    // String-table messages ride in the signon packets (before DataTables) and
    // throughout the game, so decode them regardless of whether the entity world
    // is ready yet.
    for m in csgo::scan_payload(payload) {
        match m.kind {
            csgo::MsgKind::SvcCreateStringTable => string_tables.handle_create(m.body, names),
            csgo::MsgKind::SvcUpdateStringTable => string_tables.handle_update(m.body, names),
            _ => {}
        }
    }
    let (data, world) = match (data, world) {
        (Some(d), Some(w)) => (d, w),
        _ => return,
    };
    let mut updated = false;
    for m in csgo::scan_payload(payload) {
        if m.kind == csgo::MsgKind::SvcPacketEntities
            && csgo::entities::decode_packet_entities(m.body, world, data).is_some()
        {
            updated = true;
        }
    }
    if updated {
        scrape_player_state(
            tick, world, data,
            last_pos, origin_state, last_life, last_obs, last_yaw, last_weapon,
            tracks, life_states, observer_modes, yaws, weapons, weapon_classes, econ,
            true, // CS:GO: gate Z on horizontal movement (non-local Z-drift guard)
        );
    }
}

pub(super) fn scan_game_payload(
    payload: &[u8],
    tick: i32,
    demo_protocol: i32,
    // `remap_msgs`: the engine renumbered its net-message IDs (NetSplitScreenUser
    // at 3, SvcSplitScreen at 22, SvcPrint 7→16, NetTick/StringCmd/SetConVar/
    // SignonState each −1). True for the Portal 2 engine AND L4D1/L4D2.
    // `user_msg_12bit`: svc_UserMessage's length field is 12 bits (Portal 2) vs
    // 11 (older Source incl. L4D). Independent of `remap_msgs` - L4D remaps but
    // uses 11-bit lengths.
    remap_msgs: bool,
    user_msg_12bit: bool,
    // MAX_EDICT_BITS: width of the entity-count fields in svc_PacketEntities and
    // the removed-entities list. 11 for stock Source, 13 for GMod 13 (8192 edicts).
    edict_bits: u32,
    data: Option<&DataTables>,
    mut world: Option<&mut EntityWorld>,
    last_pos: &mut HashMap<u32, (f32, f32, f32)>,
    origin_state: &mut HashMap<u32, OriginTracker>,
    last_life: &mut HashMap<u32, u8>,
    last_obs: &mut HashMap<u32, u8>,
    last_yaw: &mut HashMap<u32, (f32, f32)>,
    last_weapon: &mut HashMap<u32, i32>,
    tracks: &mut HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    life_states: &mut HashMap<u32, Vec<(i32, u8)>>,
    observer_modes: &mut HashMap<u32, Vec<(i32, u8)>>,
    yaws: &mut HashMap<u32, Vec<(i32, f32, f32)>>,
    weapons: &mut HashMap<u32, Vec<(i32, i32)>>,
    weapon_classes: &mut HashMap<i32, String>,
    econ: &mut HashMap<u32, PlayerEcon>,
    userinfo_table_id: Option<usize>,
    names: &mut HashMap<u32, PlayerInfo>,
) {
    let mut br = BitReader::new(payload);
    let total_bits = payload.len() * 8;

    macro_rules! tryread { ($e:expr) => { match $e { Some(v) => v, None => return } } }

    while br.bit_pos() + 6 <= total_bits {
        let msg_type_raw = tryread!(br.read_bits(6));
        // Portal 2 engine renumbers the message IDs. Handle its two new
        // message types inline, then remap the shifted ones back to our
        // canonical (HL2 / Source 2007) IDs so the match below is shared.
        // Reference: NeKzor/sdp NetMessages.ts Portal2Engine table. The L4D
        // engine shares this map (verified in L4D1 engine.dll), so `remap_msgs`
        // is true for both.
        if remap_msgs {
            match msg_type_raw {
                3 => { // NetSplitScreenUser: 1 bit
                    if !br.skip(1) { return; }
                    continue;
                }
                22 => { // SvcSplitScreen: 1 bit + 11-bit length + data
                    if !br.skip(1) { return; }
                    let len = tryread!(br.read_bits(11));
                    if !br.skip(len) { return; }
                    continue;
                }
                _ => {}
            }
        }
        // DUMP_MSG=1 prints every raw 6-bit message id. Pipe through
        // `sort | uniq -c | sort -rn` to histogram a new game's net-message
        // enum when porting (the once-per-packet ids are net_Tick + the
        // PacketEntities equivalent). See README "Investigating L4D2".
        if std::env::var("DUMP_MSG").is_ok() {
            eprintln!("[MSG] t={} raw={} bit_pos={}", tick, msg_type_raw, br.bit_pos());
        }
        let msg_type = if remap_msgs {
            match msg_type_raw {
                4 => 3,   // NetTick
                5 => 4,   // NetStringCmd
                6 => 5,   // NetSetConVar
                7 => 6,   // NetSignonState
                16 => 7,  // SvcPrint
                v => v,
            }
        } else { msg_type_raw };
        match msg_type {
            0  => { /* net_NOP */ }
            3  => { if !br.skip(64) { return; } } // net_Tick
            4  => { if br.read_cstring(512).is_none() { return; } } // net_StringCmd
            5  => {
                let count = tryread!(br.read_bits(8)) as usize;
                for _ in 0..count {
                    if br.read_cstring(256).is_none() || br.read_cstring(256).is_none() { return; }
                }
            }
            6  => { if !br.skip(40) { return; } } // net_SignonState
            7  => { /* svc_Print (string) */ if br.read_cstring(2048).is_none() { return; } }
            8  => { /* svc_ServerInfo - layout varies by demo_protocol.
                       Proto-3 (TF2 / CS:S net=24):
                         version(16) + server_count(32) + stv(1) + dedicated(1)
                         + max_crc(32) + max_classes(16)             = 98 bits
                         + map_hash(128) + player_slot(8) + max_players(8)
                         + interval_per_tick(32) + platform(8)       = 184 bits
                       Proto-4 (Portal 2 / L4D / Stanley, isNewEngine):
                         version(16) + server_count(32) + stv(1) + dedicated(1)
                         + max_crc(32) + max_classes(16) + map_crc(32)
                         + player_slot(8) + max_players(8) + unk(32)
                         + interval_per_tick(32) + platform(8)       = 218 bits
                       (Reference: NeKzor/sdp NetMessages.ts SvcServerInfo.) */
                let fixed = if demo_protocol >= 4 { 218 } else { 282 };
                if !br.skip(fixed) { return; }
                if br.read_cstring(260).is_none() { return; } // game
                if br.read_cstring(260).is_none() { return; } // map
                if br.read_cstring(260).is_none() { return; } // skybox
                if br.read_cstring(260).is_none() { return; } // server_name
                if !br.skip(1) { return; } // replay
            }
            9  => { /* svc_SendTable - shouldn't appear mid-game */ return; }
            10 => { /* svc_ClassInfo */
                let n = tryread!(br.read_bits(16));
                let create = tryread!(br.read_bool());
                if !create {
                    let bits = bits_for(data.map(|d| d.server_classes.len() as u32).unwrap_or(1));
                    for _ in 0..n {
                        if !br.skip(bits) { return; }
                        if br.read_cstring(256).is_none() { return; }
                        if br.read_cstring(256).is_none() { return; }
                    }
                }
            }
            11 => { if !br.skip(1) { return; } } // svc_SetPause
            12 if edict_bits == 13 => { /* GMod 13 svc_CreateStringTable - a hybrid
                       of the old and new Source layouts, verified bit-for-bit by
                       walking all 22 signon string tables to clean names
                       (downloadables, modelprecache, ..., instancebaseline,
                       userinfo, GModGameInfo) across three demos:
                         [peek 8: if ':' (0x3A) consume - a name-prefix marker]
                         name(string)
                         max_entries(16)  -> encode_bits = log2(max_entries)
                         num_entries(encode_bits + 1)
                         length = VARINT, in BITS   (NOT a 20-bit field)
                         user_data_fixed_size(1) [+ user_data_size(12) + bits(4)]
                         compressed(1)
                         data[length bits]
                       This matters only for GMod: its baseline svc_PacketEntities
                       rides in the *same* signon packet behind these tables, so the
                       walk must clear them exactly to reach the entity baseline.
                       (Older Source ships baselines in separate packets, so its
                       CreateStringTable never had to be right.) */
                let save = br.bit_pos();
                if tryread!(br.read_bits(8)) != 0x3A { br.set_bit_pos(save); }
                if br.read_cstring(256).is_none() { return; }
                let max_entries = tryread!(br.read_bits(16));
                let encode_bits = if max_entries <= 1 { 0 } else { 31 - max_entries.leading_zeros() };
                let _num_entries = tryread!(br.read_bits(encode_bits + 1));
                let length = tryread!(br.read_var_u32()) as usize;
                let user_data_fixed = tryread!(br.read_bool());
                if user_data_fixed {
                    if !br.skip(12) { return; } // user_data_size
                    if !br.skip(4) { return; }  // user_data_size_bits
                }
                let _compressed = tryread!(br.read_bool());
                if !br.skip(length as u32) { return; }
            }
            12 => { /* svc_CreateStringTable (older Source: TF2 / CS:S / HL2 / etc).
                       name(str) + max_entries(16) + num_entries(log2(max)+1)
                       + length(20) + user_data_fixed_size(1)
                         [+ user_data_size(12) + user_data_size_bits(4) if fixed]
                       + data[length]. Older Source ships entity baselines in
                       separate packets, so this never has to be exact for those
                       games (player names come from DEM_STRINGTABLES instead). */
                if br.read_cstring(256).is_none() { return; }
                let max_entries = tryread!(br.read_bits(16));
                let encode_bits = if max_entries <= 1 { 0 } else { 31 - max_entries.leading_zeros() };
                let _num_entries = tryread!(br.read_bits(encode_bits + 1));
                let length = tryread!(br.read_bits(20)) as usize;
                let user_data_fixed = tryread!(br.read_bool());
                if user_data_fixed {
                    if !br.skip(12) { return; } // user_data_size
                    if !br.skip(4) { return; }  // user_data_size_bits
                }
                if !br.skip(length as u32) { return; }
            }
            13 => { /* svc_UpdateStringTable: table_id(5) + has_changed(1)
                       + [num_changed(16) if has_changed] + length(20) + data */
                let table_id = tryread!(br.read_bits(5)) as usize;
                let has_changed = tryread!(br.read_bool());
                let num_changed = if has_changed { tryread!(br.read_bits(16)) } else { 1 };
                let length = tryread!(br.read_bits(20)) as usize;
                let data_start = br.bit_pos();
                // The userinfo table_id is known from DEM_STRINGTABLES. Only
                // userinfo carries rename-worthy data; ignore everything else.
                if Some(table_id) == userinfo_table_id {
                    apply_userinfo_update(payload, data_start, length, num_changed, names);
                }
                if !br.skip(length as u32) { return; }
            }
            14 => { /* svc_VoiceInit */
                if br.read_cstring(256).is_none() { return; }
                if !br.skip(8) { return; }
            }
            15 => {
                if !br.skip(16) { return; }
                let length = tryread!(br.read_bits(16)) as usize;
                if !br.skip(length as u32) { return; }
            }
            16 => { /* svc_HLTV control */ return; }
            17 => { /* svc_Sounds:
                       reliable(1) + (if reliable: length(8); else num(8) + length(16)) + data[length bits] */
                let reliable = tryread!(br.read_bool());
                let length = if reliable {
                    tryread!(br.read_bits(8)) as usize
                } else {
                    if !br.skip(8) { return; }
                    tryread!(br.read_bits(16)) as usize
                };
                if !br.skip(length as u32) { return; }
            }
            18 => { if !br.skip(11) { return; } }
            19 => { if !br.skip(49) { return; } }
            20 => { if !br.skip(48) { return; } }
            21 => { return; } // svc_BSPDecal
            22 => {
                if !br.skip(1) { return; }
                let length = tryread!(br.read_bits(11)) as usize;
                if !br.skip(length as u32) { return; }
            }
            23 => {
                // svc_UserMessage: msg_type(8) + length. The Portal 2 engine
                // widened the length field to 12 bits (verified against
                // SVC_UserMessage::ReadFromBuffer in engine.dll - m_nLength reads
                // `v & 0xFFF`); older Source (TF2 / CS:S, proto-3) uses 11. Read
                // the wrong width and the cursor desyncs the moment any user
                // message appears - which blanked Portal 2 speedrun demos (the
                // first user message lives in the tick-0 signon) and silently
                // killed testingportal after ~750 ticks. Gated on `user_msg_12bit`
                // (Portal 2 only) - L4D shares the net-message remap but keeps the
                // 11-bit width (its SVC_UserMessage::ReadFromBuffer reads `v &
                // 0x7FF`), and older Source (TF2 / CS:S) is untouched.
                let _ = tryread!(br.read_bits(8));
                let len_bits = if user_msg_12bit { 12 } else { 11 };
                let length = tryread!(br.read_bits(len_bits)) as usize;
                if !br.skip(length as u32) { return; }
            }
            24 => {
                if !br.skip(20) { return; }
                let length = tryread!(br.read_bits(11)) as usize;
                if !br.skip(length as u32) { return; }
            }
            25 => {
                let length = tryread!(br.read_bits(11)) as usize;
                if !br.skip(length as u32) { return; }
            }
            26 => {
                let _max_entries = tryread!(br.read_bits(edict_bits));
                let has_delta = tryread!(br.read_bool());
                if has_delta { if !br.skip(32) { return; } }
                if !br.skip(1) { return; } // base_line
                let num_changed = tryread!(br.read_bits(edict_bits));
                // svc_PacketEntities `length` (entity-data bit count): stock Source
                // = 20 bits (DELTASIZE_BITS); GMod 13 widened it to 24 along with
                // its edict limit (confirmed in engine.dll
                // SVC_PacketEntities::ReadFromBuffer). Reading 20 on GMod leaves the
                // cursor 4 bits short, desyncing every entity body.
                let length_width = if edict_bits == 13 { 24 } else { 20 };
                let length_bits = tryread!(br.read_bits(length_width)) as usize;
                if !br.skip(1) { return; } // updated_base_line
                let payload_start_bit = br.bit_pos();
                if let (Some(dt), Some(w)) = (data, world.as_deref_mut()) {
                    let r = parse_entity_updates(
                        payload, payload_start_bit, length_bits,
                        num_changed, has_delta, w, dt,
                        demo_protocol >= 4,
                        false, // interleaved index+value (bit-packed Source path)
                        edict_bits,
                        None, // prop-index follows the entity encoding (legacy here)
                    );
                    if std::env::var("DUMP_ENT").is_ok() {
                        use std::collections::BTreeMap;
                        let mut by_class: BTreeMap<u16, usize> = BTreeMap::new();
                        for s in w.entities.values() { *by_class.entry(s.class_id).or_default() += 1; }
                        let names: Vec<String> = by_class.iter().take(8).map(|(cid, n)| {
                            let nm = dt.server_classes.iter().find(|c| c.id == *cid).map(|c| c.name.as_str()).unwrap_or("?");
                            format!("{}×{}:{}", n, cid, nm)
                        }).collect();
                        eprintln!("[ENT] t={} delta={} maxE={} updates={} lenbits={} decode={} world={} eids[{}..] classes: {}",
                            tick, has_delta, _max_entries, num_changed, length_bits, if r.is_some() {"ok"} else {"NONE"},
                            w.entities.len(),
                            w.entities.keys().min().copied().unwrap_or(0),
                            names.join(" "));
                    }
                    if !br.skip(length_bits as u32) { return; }
                    scrape_player_state(tick, w, dt, last_pos, origin_state, last_life, last_obs, last_yaw, last_weapon,
                        tracks, life_states, observer_modes, yaws, weapons, weapon_classes, econ,
                        false); // bit-packed Source path: keep full Z updates
                } else {
                    if !br.skip(length_bits as u32) { return; }
                }
            }
            27 => {
                if !br.skip(9) { return; }
                let length = tryread!(br.read_bits(17)) as usize;
                if !br.skip(length as u32) { return; }
            }
            28 => { if !br.skip(14) { return; } }
            29 => {
                if !br.skip(16) { return; }
                let length = tryread!(br.read_bits(16)) as usize;
                if !br.skip(length as u32 * 8) { return; }
            }
            30 => {
                let _ = tryread!(br.read_bits(9));
                let total_length = tryread!(br.read_bits(20)) as usize;
                if !br.skip(total_length as u32) { return; }
            }
            31 => {
                if !br.skip(32) { return; }
                if br.read_cstring(256).is_none() { return; }
            }
            32 => {
                // svc_CmdKeyValues: length(32) + data[length bytes]
                let length = tryread!(br.read_bits(32)) as u32;
                if !br.skip(length.wrapping_mul(8)) { return; }
            }
            33 => {
                // svc_PaintMapData (Portal 2 family) - int32 byte length then
                // that many bits of paint data. Skip entirely.
                let length = tryread!(br.read_bits(32)) as u32;
                if !br.skip(length) { return; }
            }
            _ => return, // unknown - bail
        }
    }
}

fn bits_for(n: u32) -> u32 {
    let mut bits = 0;
    while (1u32 << bits) < n { bits += 1; }
    bits.max(1)
}

/// floor(log2(n)) for n >= 1. Used by Source for string-table entry index widths.
fn log2(n: u32) -> u32 {
    31 - n.leading_zeros()
}

/// Decode a svc_UpdateStringTable diff against the userinfo table and merge
/// any changed entries into `names`. The userinfo table in Source 1 always
/// has max_entries = 256 (= MAX_PLAYERS), so entry_bits = 8. Userdata is
/// variable-size (player_info_t).
///
/// Wire format per changed entry:
///   next_entry (1 bit) - if 1, entry = last+1; else read entry_bits absolute
///   has_string (1 bit) - if 1: substring_flag(1) + [if substring: idx(5)+nchars(5)] + cstring suffix
///   has_userdata (1 bit) - if 1: 14-bit byte count + bytes (var size)
fn apply_userinfo_update(
    payload: &[u8],
    data_start_bit: usize,
    length_bits: usize,
    num_changed: u32,
    names: &mut HashMap<u32, PlayerInfo>,
) {
    const ENTRY_BITS: u32 = 8;
    let mut br = BitReader::new(payload);
    if !br.skip(data_start_bit as u32) { return; }
    let max_pos = data_start_bit + length_bits;
    let mut last_entry: i32 = -1;
    for _ in 0..num_changed {
        if br.bit_pos() >= max_pos { return; }
        let next = match br.read_bool() { Some(b) => b, None => return };
        let entry: i32 = if next {
            last_entry + 1
        } else {
            match br.read_bits(ENTRY_BITS) { Some(v) => v as i32, None => return }
        };
        last_entry = entry;

        let has_string = match br.read_bool() { Some(b) => b, None => return };
        if has_string {
            let is_substring = match br.read_bool() { Some(b) => b, None => return };
            if is_substring {
                if !br.skip(5 + 5) { return; }
            }
            if br.read_cstring(1024).is_none() { return; }
        }

        let has_userdata = match br.read_bool() { Some(b) => b, None => return };
        if has_userdata {
            let nbytes = match br.read_bits(14) { Some(v) => v as usize, None => return };
            let mut bytes = Vec::with_capacity(nbytes);
            for _ in 0..nbytes {
                let b = match br.read_bits(8) { Some(v) => v as u8, None => return };
                bytes.push(b);
            }
            if let Some(mut pi) = super::super::stringtable::parse_player_info_blob(&bytes) {
                let entity_id = (entry as u32) + 1;
                // Preserve every prior alias for this slot, then add the new one.
                if let Some(prev) = names.get(&entity_id) {
                    pi.aliases = prev.aliases.clone();
                }
                if !pi.aliases.iter().any(|a| a == &pi.name) {
                    pi.aliases.push(pi.name.clone());
                }
                names.insert(entity_id, pi);
            }
        }
    }
}

/// Per-entity bookkeeping for picking the live m_vecOrigin source. `changes[s]`
/// counts how many times candidate slot `s` (local-exclusive vs non-local copy)
/// has actually moved; `last[s]` is its previous value for change detection.
#[derive(Default, Clone)]
pub struct OriginTracker {
    last: Vec<Option<(f32, f32, f32)>>,
    changes: Vec<u32>,
}

#[allow(clippy::too_many_arguments)]
fn scrape_player_state(
    tick: i32,
    world: &EntityWorld,
    data: &DataTables,
    last_pos: &mut HashMap<u32, (f32, f32, f32)>,
    origin_state: &mut HashMap<u32, OriginTracker>,
    last_life: &mut HashMap<u32, u8>,
    last_obs: &mut HashMap<u32, u8>,
    last_yaw: &mut HashMap<u32, (f32, f32)>,
    last_weapon: &mut HashMap<u32, i32>,
    tracks: &mut HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    life_states: &mut HashMap<u32, Vec<(i32, u8)>>,
    observer_modes: &mut HashMap<u32, Vec<(i32, u8)>>,
    yaws: &mut HashMap<u32, Vec<(i32, f32, f32)>>,
    weapons: &mut HashMap<u32, Vec<(i32, i32)>>,
    weapon_classes: &mut HashMap<i32, String>,
    econ: &mut HashMap<u32, PlayerEcon>,
    // CS:GO's non-local m_vecOrigin[2] (Z) ramps to garbage once an entity stops
    // updating (the X/Y stay put while Z climbs ~4 units/tick into the sky). When
    // set, only fold a new Z in alongside horizontal movement, so a stationary or
    // out-of-PVS player's track freezes at its last real position instead.
    gate_z_on_xy: bool,
) {
    for (&eid, state) in &world.entities {
        if eid == 0 || eid > 64 { continue; }
        let class = match data.server_classes.iter().find(|c| c.id == state.class_id) {
            Some(c) => c,
            None => continue,
        };
        if !class.name.contains("Player") { continue; }
        let flat = match data.flat_props.get(&state.class_id) {
            Some(f) => f,
            None => continue,
        };

        // Find m_vecOrigin / m_vecOrigin[2] / m_lifeState / m_angEyeAngles[1]
        // / m_hActiveWeapon by name in the flat list. The eye-angles yaw is
        // what drives WASD input direction in Source. m_hActiveWeapon points
        // at the entity id of the wielded weapon - we resolve that to a class
        // name on the HTML side.
        let mut life = None;
        let mut yaw = None;
        let mut pitch = None;
        let mut wep_handle = None;
        // Economy/scoreboard fields that live directly on the player entity
        // (CCSPlayer): cash on hand and team. Kills/deaths/score come off the
        // separate CCSPlayerResource entity, handled after this loop.
        let mut money = None;
        let mut team = None;
        // m_iObserverMode: 0 = not spectating; anything else = dead/spectating
        // (deathcam, chase, roaming, …). While observing, the engine streams
        // m_vecOrigin as the *spectated* target's position, so the value is
        // meaningless for this player - the HTML uses this stream to break the
        // path line and hide the avatar during those windows.
        let mut obs = None;
        // A player class carries more than one m_vecOrigin: the local-player-
        // exclusive copy (DT_LocalPlayerExclusive, earlier in the flat list)
        // and a non-local copy. The server only streams ONE of them per
        // entity - the local-exclusive one for the recorder, the non-local
        // one for everyone else - while the other stays frozen at its baseline
        // value. We can't tell which is live from a single tick (both are
        // present), so collect every origin candidate in flat order here and
        // let the per-entity tracker below pick whichever one is actually
        // moving. (Blindly taking the first froze all non-local players.)
        let mut origin_cands: Vec<(Option<f32>, Option<f32>, Option<f32>)> = Vec::new();
        for (i, p) in flat.iter().enumerate() {
            match p.name.as_str() {
                "m_vecOrigin" => {
                    let mut c = (None, None, None);
                    if let Some(v) = state.props.get(&i) {
                        if let Some((vx, vy)) = v.as_vector_xy() {
                            c.0 = Some(vx); c.1 = Some(vy);
                        } else if let Some((vx, vy, vz)) = v.as_vector() {
                            c = (Some(vx), Some(vy), Some(vz));
                        }
                    }
                    origin_cands.push(c);
                }
                "m_vecOrigin[2]" => {
                    // Pairs with the most recent m_vecOrigin (10↔11, 14↔15, …).
                    if let Some(v) = state.props.get(&i) {
                        if let Some(f) = v.as_f32() {
                            if let Some(last) = origin_cands.last_mut() { last.2 = Some(f); }
                        }
                    }
                }
                "m_lifeState" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(n) = v.as_i64() { life = Some(n as u8); }
                    }
                }
                "m_angEyeAngles[1]" | "m_angRotation[1]" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(f) = v.as_f32() { yaw = Some(f); }
                    }
                }
                // Pitch (look up/down). Needed to drive the first-person camera
                // on proto-4 demos, which have no usercmds - the playback
                // timeline is synthesized from this + yaw + the position track.
                "m_angEyeAngles[0]" | "m_angRotation[0]" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(f) = v.as_f32() { pitch = Some(f); }
                    }
                }
                "m_iObserverMode" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(n) = v.as_i64() { obs = Some(n as u8); }
                    }
                }
                "m_hActiveWeapon" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(n) = v.as_i64() {
                            // EHANDLE: low 11 bits = entity index; serial in upper bits.
                            let ent_idx = (n as i32) & 0x7FF;
                            wep_handle = Some(ent_idx);
                        }
                    }
                }
                "m_iAccount" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(n) = v.as_i64() { money = Some(n as i32); }
                    }
                }
                "m_iTeamNum" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(n) = v.as_i64() { team = Some(n as i32); }
                    }
                }
                _ => {}
            }
        }

        let eid_u = eid as u32;

        // Pick the live origin source for this entity. We score each candidate
        // slot by how many times its value has changed so far; the server only
        // keeps one slot moving, so it pulls ahead within a few ticks and the
        // argmax locks onto it. (The first time a slot is seen doesn't count as
        // a change, so a baseline-only copy stays at zero and never wins.) On a
        // tie - including the very first tick, where both copies share the
        // baseline value - we prefer the earliest slot, matching the old
        // local-player behaviour.
        let tracker = origin_state.entry(eid_u).or_default();
        if tracker.last.len() < origin_cands.len() {
            tracker.last.resize(origin_cands.len(), None);
            tracker.changes.resize(origin_cands.len(), 0);
        }
        for (s, c) in origin_cands.iter().enumerate() {
            if let (Some(cx), Some(cy)) = (c.0, c.1) {
                let cz = c.2.unwrap_or(0.0);
                // Score a slot as "moved" only on horizontal (X/Y) change. Player
                // locomotion is horizontal; the dormant origin copy can drift in
                // Z alone (a stale/baseline m_vecOrigin[2] — seen on CS:GO's
                // non-local players, whose Z ramps to garbage while X/Y stay put),
                // and counting that as movement would lock onto the wrong copy.
                let moved = tracker.last[s].map_or(false, |(lx, ly, _lz): (f32, f32, f32)| {
                    (lx - cx).abs() > 0.01 || (ly - cy).abs() > 0.01
                });
                if moved { tracker.changes[s] += 1; }
                tracker.last[s] = Some((cx, cy, cz));
            }
        }
        let mut best = 0usize;
        for s in 1..tracker.changes.len() {
            if tracker.changes[s] > tracker.changes[best] { best = s; }
        }
        let (x, y, z) = match origin_cands.get(best) {
            Some(&(cx, cy, cz)) => (cx, cy, cz),
            None => (None, None, None),
        };

        let pos = last_pos.entry(eid_u).or_insert((0.0, 0.0, 0.0));
        let mut xy_changed = false;
        if let Some(vx) = x { if pos.0 != vx { pos.0 = vx; xy_changed = true; } }
        if let Some(vy) = y { if pos.1 != vy { pos.1 = vy; xy_changed = true; } }
        // Fold Z in unconditionally, except under `gate_z_on_xy` (CS:GO) where a
        // lone Z change with frozen X/Y is the non-local Z-drift artifact — there
        // we only accept Z while moving horizontally.
        let mut changed = xy_changed;
        if !(gate_z_on_xy && !xy_changed) {
            if let Some(vz) = z { if pos.2 != vz { pos.2 = vz; changed = true; } }
        }

        if changed {
            let mag2 = pos.0*pos.0 + pos.1*pos.1 + pos.2*pos.2;
            let bucket = tracks.entry(eid_u).or_default();
            let near_origin = mag2 < 16.0;
            let dedupe = bucket.last().map_or(false, |&(_, lx, ly, lz)| {
                let dx = pos.0 - lx; let dy = pos.1 - ly; let dz = pos.2 - lz;
                dx*dx + dy*dy + dz*dz < 1.0
            });
            if !(near_origin && bucket.is_empty()) && !dedupe {
                bucket.push((tick, pos.0, pos.1, pos.2));
            }
        }

        if let Some(ls) = life {
            if last_life.get(&eid_u).copied() != Some(ls) {
                last_life.insert(eid_u, ls);
                life_states.entry(eid_u).or_default().push((tick, ls));
            }
        }

        // Observer-mode transitions (analogous to life-state). Emitted only on
        // change so the stream stays tiny.
        if let Some(om) = obs {
            if last_obs.get(&eid_u).copied() != Some(om) {
                last_obs.insert(eid_u, om);
                observer_modes.entry(eid_u).or_default().push((tick, om));
            }
        }

        // Eye angles (yaw + pitch) - dedupe small changes (< 2° on either axis)
        // since the prop fires for every tiny mouse movement and would
        // otherwise be megabytes of noise. Emitted as (tick, yaw, pitch).
        if let Some(y_now) = yaw {
            let p_now = pitch.unwrap_or(0.0);
            let prev = last_yaw.get(&eid_u).copied();
            let should_emit = prev.map_or(true, |(py, pp)| (py - y_now).abs() >= 2.0 || (pp - p_now).abs() >= 2.0);
            if should_emit {
                last_yaw.insert(eid_u, (y_now, p_now));
                yaws.entry(eid_u).or_default().push((tick, y_now, p_now));
            }
        }

        // Active weapon entity id. Only emit on change to keep the stream
        // tiny - switching weapons is rare compared to ticks. Also record the
        // weapon entity's class name once seen, so the HTML side can resolve
        // ids → human-readable names like "CTFRocketLauncher".
        if let Some(w) = wep_handle {
            if last_weapon.get(&eid_u).copied() != Some(w) {
                last_weapon.insert(eid_u, w);
                weapons.entry(eid_u).or_default().push((tick, w));
                if w > 0 && w < u16::MAX as i32 && !weapon_classes.contains_key(&w) {
                    if let Some(wstate) = world.entities.get(&(w as u16)) {
                        if let Some(c) = data.server_classes.iter().find(|c| c.id == wstate.class_id) {
                            weapon_classes.insert(w, c.name.clone());
                        }
                    }
                }
            }
        }

        // Player-entity economy (cash + team). Latest value wins; only touch the
        // econ map once we've actually seen one of these props, so non-CS player
        // classes (and the CCSPlayerResource entity) never spawn empty entries.
        if money.is_some() || team.is_some() {
            let e = econ.entry(eid_u).or_default();
            if let Some(m) = money { e.money = m; }
            if let Some(t) = team { e.team = t; }
        }
    }

    scrape_resource_scoreboard(world, data, econ);
}

/// Read the per-player scoreboard arrays off the singleton `CCSPlayerResource`
/// entity into `econ`, keyed by player entity-index. On Source 1 these stats
/// (score / kills / deaths / assists / MVPs) live on a resource entity as
/// engine-generated `SendPropArray`s whose leaves flatten to bare slot indices
/// ("000".."063") with the array name recovered via `array_parent`. CS:S ships
/// only `m_iScore` (the frag count), so kills falls back to score there.
fn scrape_resource_scoreboard(
    world: &EntityWorld,
    data: &DataTables,
    econ: &mut HashMap<u32, PlayerEcon>,
) {
    let resource = world.entities.values().find(|s| {
        data.server_classes.iter().any(|c| c.id == s.class_id && c.name == "CCSPlayerResource")
    });
    let Some(state) = resource else { return };
    let Some(flat) = data.flat_props.get(&state.class_id) else { return };

    let has_kills = flat.iter().any(|p| p.array_parent.as_deref() == Some("m_iKills"));
    let mut touched: Vec<u32> = Vec::new();
    for (i, p) in flat.iter().enumerate() {
        let Some(parent) = p.array_parent.as_deref() else { continue };
        // The leaf name is the per-player slot = player entity index (1..64).
        let Ok(slot) = p.name.parse::<u32>() else { continue };
        if slot == 0 || slot > 64 { continue; }
        let Some(v) = state.props.get(&i).and_then(|v| v.as_i64()) else { continue };
        let v = v as i32;
        let e = econ.entry(slot).or_default();
        match parent {
            "m_iScore"   => { e.score = v; touched.push(slot); }
            "m_iKills"   => e.kills = v,
            "m_iDeaths"  => e.deaths = v,
            "m_iAssists" => e.assists = v,
            "m_iMVPs"    => e.mvps = v,
            // m_iTeam mirrors the player entity's m_iTeamNum; only fill in if the
            // player-entity pass hasn't (resource is always present, players may
            // be out of PVS).
            "m_iTeam"    => { if e.team == 0 { e.team = v; } }
            _ => {}
        }
    }
    // CS:S has no per-player kills array; its score IS the frag count.
    if !has_kills {
        for slot in touched {
            if let Some(e) = econ.get_mut(&slot) {
                if e.kills == 0 { e.kills = e.score; }
            }
        }
    }
}
