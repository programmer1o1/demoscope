// Game-event extraction: the bit-packed net-message walker that pulls
// svc_GameEvent / SayText2 out of each game packet, the GameEventList schema
// scan that tells it how to decode them, and the derived life/teleport break
// computation. Also holds SampledCmd (the per-tick input row) since the break
// logic indexes into it.

use std::collections::{HashMap, HashSet};

use super::super::util::bitreader::BitReader;

pub(crate) struct SampledCmd {
    pub(crate) tick: i32,
    pub(crate) pitch: f32,
    pub(crate) yaw: f32,
    pub(crate) fwd: f32,
    pub(crate) side: f32,
    pub(crate) btns: u32,
    pub(crate) weapon: u32,
}

pub(crate) enum EventValue {
    Str(String),
    Float(f32),
    Int(i32),
    Bool(bool),
    Null,
}

pub(crate) struct EventField {
    pub(crate) name: String,
    pub(crate) value: EventValue,
}

pub(crate) struct GameEvent {
    pub(crate) event: String,
    pub(crate) tick: i32,
    pub(crate) fields: Vec<EventField>,
}

pub(crate) struct EventSchema {
    pub(crate) name: String,
    pub(crate) fields: Vec<(String, u8)>, // (name, type: 1=str,2=float,3=long,4=short,5=byte,6=bool,7=local)
}

pub(crate) fn scan_for_game_event_list(payload: &[u8]) -> Option<HashMap<u16, EventSchema>> {
    if payload.len() < 20 {
        return None;
    }
    let total_bits = payload.len() * 8;
    let limit = total_bits.min(50_000);

    for start in 0..limit.saturating_sub(40) {
        // Quick check: read 6 bits at `start`, must be 30
        let byte_idx = start >> 3;
        let bit_off = start & 7;
        if byte_idx + 1 >= payload.len() {
            break;
        }
        let raw = (payload[byte_idx] as u32) | ((payload[byte_idx + 1] as u32) << 8);
        let msg_type = (raw >> bit_off) & 0x3F;
        if msg_type != 30 {
            continue;
        }

        // Try to parse event list starting after the 6-bit type
        if let Some(schema) = try_parse_event_list(payload, start + 6) {
            return Some(schema);
        }
    }
    None
}

fn try_parse_event_list(payload: &[u8], start_bit: usize) -> Option<HashMap<u16, EventSchema>> {
    let mut br = BitReader::new_at(payload, start_bit);

    let count = br.read_bits(9)? as usize;
    let total_length = br.read_bits(20)? as usize;

    if count < 10 || count > 512 {
        return None;
    }
    if total_length < count * 20 || total_length > 200_000 {
        return None;
    }

    let end_bit = br.bit_pos + total_length;
    let mut schemas: HashMap<u16, EventSchema> = HashMap::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    for _ in 0..count {
        if br.bit_pos >= end_bit {
            return None;
        }
        let eid = br.read_bits(9)? as u16;
        let name = br.try_read_cstring(64)?;
        if name.is_empty() {
            return None;
        }
        seen_names.insert(name.clone());

        let mut fields: Vec<(String, u8)> = Vec::new();
        loop {
            if br.bit_pos >= end_bit + 64 {
                return None;
            }
            if br.bits_remaining() < 3 {
                return None;
            }
            let ftype = br.read_bits(3)? as u8;
            if ftype == 0 {
                break;
            }
            let fname = br.read_cstring_any(64)?;
            fields.push((fname, ftype));
        }

        schemas.insert(eid, EventSchema { name, fields });
    }

    if !seen_names.contains("player_death") {
        return None;
    }

    Some(schemas)
}

pub(crate) fn extract_events_from_payload(
    payload: &[u8],
    tick: i32,
    schemas: &HashMap<u16, EventSchema>,
    display_events: &HashSet<&str>,
    // Proto-4 engines renumber the net messages (see the remap below).
    // `remap_msgs` = Portal 2 engine || L4D; `user_msg_12bit` = Portal 2 only.
    remap_msgs: bool,
    user_msg_12bit: bool,
    demo_protocol: i32,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut br = BitReader::new(payload);
    let total_bits = payload.len() * 8;

    while br.bit_pos + 6 <= total_bits {
        let msg_type_raw = match br.read_bits(6) {
            Some(v) => v,
            None => break,
        };

        // Proto-4 engines (Portal 2 / Stanley / L4D) renumber the net messages:
        // NetSplitScreenUser is inserted at 3, SvcSplitScreen at 22, SvcPrint
        // moves 7→16, and NetTick/StringCmd/SetConVar/SignonState each shift
        // down one. Mirror the remap in
        // source_demo::player_tracks::scan_game_payload so the match arms below
        // stay on canonical (proto-3) IDs and svc_GameEvent (25) is reached.
        // (Verified against L4D1 engine.dll; see docs/PROTO4.md.)
        if remap_msgs {
            match msg_type_raw {
                3 => { if !br.skip(1) { break; } continue; } // NetSplitScreenUser
                22 => { // SvcSplitScreen: 1 bit + 11-bit length + data
                    if !br.skip(1) { break; }
                    let len = match br.read_bits(11) { Some(v) => v, None => break };
                    if !br.skip(len) { break; }
                    continue;
                }
                _ => {}
            }
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
        } else {
            msg_type_raw
        };

        match msg_type {
            0 => {} // net_NOP

            3 => {
                // net_Tick: tick(32) + host_frametime(16) + frametime_stddev(16) = 64 bits
                if !br.skip(64) { break; }
            }

            4 => {
                // net_StringCmd: null-terminated string
                if br.read_cstring_any(512).is_none() { break; }
            }

            5 => {
                // net_SetConVar: count(8) + count×(key_str + val_str)
                let count = match br.read_bits(8) { Some(v) => v as usize, None => break };
                let mut ok = true;
                for _ in 0..count {
                    if br.read_cstring_any(256).is_none() || br.read_cstring_any(256).is_none() {
                        ok = false; break;
                    }
                }
                if !ok { break; }
            }

            6 => {
                // net_SignonState: state(8) + spawn_count(32) = 40 bits
                if !br.skip(40) { break; }
            }

            7 => {
                // svc_Print: null-terminated string. On proto-4 this is the
                // remapped SvcPrint (raw id 16); without an arm here the scan
                // would bail at every Print and miss the events after it.
                if br.read_cstring_any(2048).is_none() { break; }
            }

            8 => {
                // svc_ServerInfo: fixed prefix (proto-4 218 bits / proto-3 282)
                // + game/map/skybox/server_name cstrings + 1 bit. Framing copied
                // from player_tracks::scan_game_payload. Normally signon-only.
                let fixed = if demo_protocol >= 4 { 218 } else { 282 };
                if !br.skip(fixed) { break; }
                if br.read_cstring_any(260).is_none() { break; }
                if br.read_cstring_any(260).is_none() { break; }
                if br.read_cstring_any(260).is_none() { break; }
                if br.read_cstring_any(260).is_none() { break; }
                if !br.skip(1) { break; }
            }

            11 => {
                // svc_SetPause: paused(1)
                if !br.skip(1) { break; }
            }

            13 => {
                // svc_UpdateStringTable: table_id(5) + has_changed(1)
                // + [num_changed(16) if has_changed] + length(20) + data.
                // Appears in proto-4 game packets ahead of svc_GameEvent, so the
                // walker must skip it cleanly rather than bail. Framing matches
                // player_tracks::scan_game_payload.
                if !br.skip(5) { break; }
                let has_changed = match br.read_bits(1) { Some(v) => v != 0, None => break };
                if has_changed { if !br.skip(16) { break; } }
                let length = match br.read_bits(20) { Some(v) => v as usize, None => break };
                if !br.skip(length as u32) { break; }
            }

            14 => {
                // svc_VoiceInit: codec string + quality(8)
                if br.read_cstring_any(256).is_none() { break; }
                if !br.skip(8) { break; }
            }

            15 => {
                // svc_VoiceData: client(8) + proximity(8) + length_bits(16) + data[length]
                if !br.skip(16) { break; }
                let length = match br.read_bits(16) { Some(v) => v as usize, None => break };
                if !br.skip(length as u32) { break; }
            }

            17 => {
                // svc_Sounds: reliable(1) + (reliable ? length(8) : num(8)+length(16))
                // + data[length bits]. This leads most proto-4 game packets, so
                // breaking here (the old behaviour) dropped every later message -
                // including svc_GameEvent. Framing matches player_tracks.
                let reliable = match br.read_bits(1) { Some(v) => v != 0, None => break };
                let length = if reliable {
                    match br.read_bits(8) { Some(v) => v as usize, None => break }
                } else {
                    if !br.skip(8) { break; }
                    match br.read_bits(16) { Some(v) => v as usize, None => break }
                };
                if !br.skip(length as u32) { break; }
            }

            18 => {
                // svc_SetView: entity_index(11)
                if !br.skip(11) { break; }
            }

            19 => {
                // svc_FixAngle: relative(1) + angle_x(16) + angle_y(16) + angle_z(16) = 49
                if !br.skip(49) { break; }
            }

            20 => {
                // svc_CrosshairAngle: angle_x(16) + angle_y(16) + angle_z(16) = 48
                if !br.skip(48) { break; }
            }

            21 => {
                // svc_BSPDecal: complex variable-length; stop packet scan
                break;
            }

            22 => {
                // svc_SplitScreen: type(1) + length(11) + data[length]
                if !br.skip(1) { break; }
                let length = match br.read_bits(11) { Some(v) => v as usize, None => break };
                if !br.skip(length as u32) { break; }
            }

            23 => {
                // svc_UserMessage: msg_type(8) + length_bits + data[length].
                // The Portal 2 engine widened the length field to 12 bits
                // (engine.dll SVC_UserMessage::ReadFromBuffer reads `v & 0xFFF`);
                // older Source (TF2 / CS:S) and L4D use 11. Matches the
                // `user_msg_12bit` handling in player_tracks::scan_game_payload.
                let msg_type = match br.read_bits(8) { Some(v) => v, None => break };
                let len_bits = if user_msg_12bit { 12 } else { 11 };
                let length   = match br.read_bits(len_bits) { Some(v) => v as usize, None => break };
                let save = br.bit_pos;
                if (msg_type == 3 || msg_type == 4) && length >= 16 {
                    if let Some(ev) = try_parse_say_text2(&mut br, tick) {
                        if display_events.contains(ev.event.as_str()) {
                            events.push(ev);
                        }
                    }
                }
                br.bit_pos = save + length;
            }

            24 => {
                // svc_EntityMessage: entity_index(11) + class_id(9) + length(11) + data[length]
                if !br.skip(20) { break; }
                let length = match br.read_bits(11) { Some(v) => v as usize, None => break };
                if !br.skip(length as u32) { break; }
            }

            25 => {
                // svc_GameEvent
                if br.bit_pos + 11 + 9 > total_bits { break; }
                let length = match br.read_bits(11) {
                    Some(v) => v as usize,
                    None => break,
                };
                let save_pos = br.bit_pos;
                let eid = match br.read_bits(9) {
                    Some(v) => v as u16,
                    None => {
                        br.bit_pos = save_pos + length;
                        continue;
                    }
                };

                if let Some(schema) = schemas.get(&eid) {
                    if display_events.contains(schema.name.as_str()) {
                        if let Some(fields) = parse_event_fields_bits(&mut br, &schema.fields) {
                            events.push(GameEvent {
                                event: schema.name.clone(),
                                tick,
                                fields,
                            });
                        }
                    }
                }
                br.bit_pos = save_pos + length;
            }

            26 => {
                // svc_PacketEntities (OB/TF2 format)
                // max_entries(11) + is_delta(1) + [delta_from(32)] +
                // update_baseline(1) + num_changed(11) + length(20) + has_multi_origins(1) + data[length]
                if br.read_bits(11).is_none() { break; }  // max_entries
                let is_delta = match br.read_bits(1) { Some(v) => v != 0, None => break };
                if is_delta {
                    if !br.skip(32) { break; }  // delta_from tick
                }
                if !br.skip(1)  { break; }  // update_baseline
                if !br.skip(11) { break; }  // num_changed_entities
                let length = match br.read_bits(20) { Some(v) => v as usize, None => break };
                if !br.skip(1) { break; }   // has_multiple_origins
                if br.bit_pos + length > total_bits { break; }
                br.bit_pos += length;
            }

            27 => {
                // svc_TempEntities: reliable(1) + count(8) + length(17) + data[length]
                if !br.skip(9) { break; }
                let length = match br.read_bits(17) { Some(v) => v as usize, None => break };
                if !br.skip(length as u32) { break; }
            }

            28 => {
                // svc_Prefetch: type(1) + sound_index(13) = 14 bits
                if !br.skip(14) { break; }
            }

            29 => {
                // svc_Menu: dialog_type(16) + data_length_bytes(16) + data
                if !br.skip(16) { break; }
                let length = match br.read_bits(16) { Some(v) => v as usize, None => break };
                if !br.skip(length as u32 * 8) { break; }
            }

            30 => {
                // svc_GameEventList - skip
                if br.bit_pos + 29 > total_bits { break; }
                if br.read_bits(9).is_none() { break; }
                let total_length = match br.read_bits(20) {
                    Some(v) => v as usize,
                    None => break,
                };
                if br.bit_pos + total_length > total_bits { break; }
                br.bit_pos += total_length;
            }

            31 => {
                // svc_GetCvarValue: cookie(32) + cvar_name(string)
                if !br.skip(32) { break; }
                if br.read_cstring_any(256).is_none() { break; }
            }

            32 => {
                // svc_CmdKeyValues (Portal 2 family): length(32) + data[length bytes]
                let length = match br.read_bits(32) { Some(v) => v, None => break };
                if !br.skip(length.wrapping_mul(8)) { break; }
            }

            33 => {
                // svc_PaintMapData (Portal 2 family): length(32) bits of paint data
                let length = match br.read_bits(32) { Some(v) => v, None => break };
                if !br.skip(length) { break; }
            }

            _ => break,
        }
    }

    events
}

fn try_parse_say_text2(br: &mut BitReader, tick: i32) -> Option<GameEvent> {
    let _client = br.read_bits(8)?;
    let _raw    = br.read_bits(8)?;
    let msg_name    = br.read_cstring_any(64)?;
    let player_name = br.read_cstring_any(64)?;
    let message     = br.read_cstring_any(256)?;
    if msg_name.is_empty() { return None; }
    if !msg_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':') { return None; }
    if player_name.is_empty() && message.is_empty() { return None; }
    let is_team = msg_name.contains("Team") || msg_name.contains("team");
    let ev_name = if is_team { "say_team" } else { "say" };
    Some(GameEvent {
        event: ev_name.to_string(),
        tick,
        fields: vec![
            EventField { name: "player".to_string(), value: EventValue::Str(player_name) },
            EventField { name: "text".to_string(),   value: EventValue::Str(message) },
        ],
    })
}

fn parse_event_fields_bits(
    br: &mut BitReader,
    fields: &[(String, u8)],
) -> Option<Vec<EventField>> {
    let mut result = Vec::new();
    for (fname, ftype) in fields {
        let value = match ftype {
            1 => {
                // string
                let s = br.read_cstring_any(256)?;
                EventValue::Str(s)
            }
            2 => {
                // float
                let f = br.read_bit_float()?;
                EventValue::Float(f)
            }
            3 => {
                // long (32-bit signed)
                let v = br.read_bits(32)? as i32;
                EventValue::Int(v)
            }
            4 => {
                // short (16-bit signed)
                let v = br.read_bits(16)? as i16 as i32;
                EventValue::Int(v)
            }
            5 => {
                // byte
                let v = br.read_bits(8)? as i32;
                EventValue::Int(v)
            }
            6 => {
                // bool
                let v = br.read_bits(1)? != 0;
                EventValue::Bool(v)
            }
            7 => {
                // local (no bits)
                EventValue::Null
            }
            _ => EventValue::Null,
        };
        result.push(EventField { name: fname.clone(), value });
    }
    Some(result)
}

// ── Life/teleport break computation ──────────────────────────────────────────

fn get_int_field(event: &GameEvent, name: &str) -> Option<i32> {
    event.fields.iter()
        .find(|f| f.name == name)
        .and_then(|f| match &f.value {
            EventValue::Int(v) => Some(*v),
            _ => None,
        })
}

pub(crate) fn compute_life_breaks(
    cmds: &[SampledCmd],
    events: &[GameEvent],
) -> (Vec<usize>, Vec<usize>) {
    if cmds.is_empty() {
        return (vec![], vec![]);
    }

    let find_idx = |tick: i32| -> usize {
        let mut lo = 0usize;
        let mut hi = cmds.len() - 1;
        while lo < hi {
            let mid = (lo + hi) >> 1;
            if cmds[mid].tick < tick {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    };

    // Identify the local player's userid as the most frequently spawning userid.
    // This prevents another player's death from creating a spurious life break.
    let local_uid: Option<i32> = {
        let mut counts: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
        for e in events.iter().filter(|e| e.event == "player_spawn") {
            if let Some(uid) = get_int_field(e, "userid") {
                *counts.entry(uid).or_insert(0) += 1;
            }
        }
        counts.into_iter().max_by_key(|&(_, c)| c).map(|(id, _)| id)
    };

    let uid_matches = |e: &GameEvent| -> bool {
        match local_uid {
            Some(uid) => get_int_field(e, "userid") == Some(uid),
            None => true, // no userid info - include all
        }
    };

    let mut spawn_events: Vec<i32> = events.iter()
        .filter(|e| e.event == "player_spawn" && uid_matches(e))
        .map(|e| e.tick)
        .collect();
    spawn_events.sort_unstable();

    let mut death_events: Vec<i32> = events.iter()
        .filter(|e| e.event == "player_death" && uid_matches(e))
        .map(|e| e.tick)
        .collect();
    death_events.sort_unstable();

    let mut tele_events: Vec<i32> = events.iter()
        .filter(|e| e.event == "player_teleported")
        .map(|e| e.tick)
        .collect();
    tele_events.sort_unstable();

    // Real respawns: player_spawn preceded by a death within 30s (1980 ticks)
    let mut real_respawns: Vec<i32> = Vec::new();
    for &sp in spawn_events.iter().skip(1) {
        let has_prior_death = death_events.iter().any(|&dt| sp - dt > 0 && sp - dt < 1980);
        if has_prior_death {
            real_respawns.push(sp);
        }
    }

    let respawn_source = if !real_respawns.is_empty() {
        real_respawns.clone()
    } else {
        death_events.clone()
    };

    let respawn_idxs: Vec<usize> = respawn_source.iter().map(|&t| find_idx(t)).collect();
    let teleport_idxs: Vec<usize> = tele_events.iter().map(|&t| find_idx(t)).collect();

    let mut all_set: HashSet<usize> = HashSet::new();
    for &i in &respawn_idxs { all_set.insert(i); }
    for &i in &teleport_idxs { all_set.insert(i); }

    let mut all_breaks: Vec<usize> = all_set.into_iter().collect();
    all_breaks.sort_unstable();

    (all_breaks, teleport_idxs)
}

pub(crate) fn display_events_for_game(game_dir: &str) -> HashSet<&'static str> {
    let mut s = HashSet::new();
    // Common to all Source games
    s.insert("player_death");
    s.insert("player_spawn");
    s.insert("player_hurt");
    s.insert("console_cmd");
    s.insert("say");
    s.insert("say_team");

    match game_dir {
        "tf" => {
            s.insert("player_teleported");
            s.insert("teamplay_round_start");
            s.insert("teamplay_round_active");
            s.insert("teamplay_round_win");
            s.insert("teamplay_game_over");
            s.insert("player_chargedeployed");
            s.insert("player_jarated");
            s.insert("player_stunned");
            s.insert("player_healonhit");
            s.insert("player_calledformedic");
            s.insert("player_stealsandvich");
            s.insert("mvm_begin_wave");
            s.insert("mvm_wave_complete");
            s.insert("mvm_wave_failed");
            s.insert("rocket_jump");
            s.insert("rocket_jump_landed");
            s.insert("sticky_jump");
            s.insert("sticky_jump_landed");
            s.insert("teamplay_flag_event");
            s.insert("teamplay_point_captured");
            s.insert("teamplay_capture_blocked");
            s.insert("teamplay_overtime_begin");
            s.insert("teamplay_suddendeath_begin");
        }
        "cstrike" | "csgo" => {
            s.insert("bomb_planted");
            s.insert("bomb_defused");
            s.insert("bomb_exploded");
            s.insert("bomb_beginplant");
            s.insert("bomb_abortplant");
            s.insert("bomb_begindefuse");
            s.insert("round_start");
            s.insert("round_end");
            s.insert("round_mvp");
            s.insert("round_freeze_end");
            s.insert("weapon_fire");
            s.insert("weapon_reload");
            s.insert("player_blind");
            s.insert("player_falldamage");
            s.insert("hostage_rescued");
            s.insert("cs_win_panel_match");
            // Grenade detonations — carry the detonation x/y/z + thrower, so the
            // viewer can mark where each landed (same overlay as CS2/CS:GO).
            s.insert("hegrenade_detonate");
            s.insert("flashbang_detonate");
            s.insert("smokegrenade_detonate");
            s.insert("molotov_detonate");
            s.insert("inferno_startburn");
            s.insert("decoy_started");
        }
        "left4dead" | "left4dead2" => {
            s.insert("round_start");
            s.insert("round_end");
            s.insert("player_incapacitated");
            s.insert("player_ledge_grab");
            s.insert("survivor_rescued");
            s.insert("tank_spawn");
            s.insert("witch_spawn");
            s.insert("player_now_it");
            s.insert("player_entered_safe_area");
        }
        "portal" | "portal2" => {
            s.insert("portal_fired");
            s.insert("portal_player_touchingportal");
            s.insert("challenge_mode_start_timer");
            s.insert("challenge_mode_close_all_gates");
        }
        "hl2mp" => {
            s.insert("round_start");
            s.insert("round_end");
            s.insert("hl2mp_awards");
        }
        _ => {
            s.insert("round_start");
            s.insert("round_end");
        }
    }
    s
}
