// Non-Source HTML generators: the Quake-family (Q1/Q2/Q3) and GoldSrc (HL1)
// paths. Both fill the SAME `template.html` as the Source generator in the
// parent `html` module, reusing its JSON serializers and the shared BSP
// dispatcher, so the 3D viewer consumes their output identically.

use std::path::Path;

use super::super::super::bsp::{extract_any_bsp, extract_goldsrc_bsp_from_bytes, find_vpk_file};
use super::super::super::{goldsrc, quake, source2};
use super::super::json::{
    escape_html, escape_json_str, events_to_json, json_f32, meta_to_json, multi_life_states_to_json,
    multi_names_to_json, multi_observer_modes_to_json, multi_tracks_to_json,
    multi_weapon_classes_to_json, multi_weapons_to_json, multi_yaws_to_json, spawn_to_json,
    view_angles_to_json, world_positions_to_json,
};
use super::HTML_TEMPLATE;

// Quake-family HTML generator: parses a Q1/Q2/Q3 demo into a MultiPlayerData
// and fills the SAME template.html as the Source path, leaving Source-only
// sections (usercmds, events, teleport/life breaks) empty. The 3D viewer,
// minimap, heatmap, player sidebar, and POV camera all work from the tracks.
// `bsp_bytes` is the matching `.bsp` (Q1 v29 / Q2 IBSP38 / Q3 IBSP46) if it was
// found beside the demo, decoded via the shared dispatcher.
pub fn generate_quake_html(
    data: &[u8],
    kind: quake::QuakeKind,
    bsp_bytes: Option<&[u8]>,
    name_hint: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    eprintln!("  Quake demo: kind={:?}", kind);
    let demo = quake::parse(kind, data, name_hint)?;
    let meta = &demo.meta;
    let mpd = &demo.mpd;

    // The primary (recorder) entity's track doubles as the single-POV
    // WORLD_POSITIONS path that drives the speedometer and fallback camera.
    let world_positions: Vec<(i32, f32, f32, f32)> = mpd
        .primary_entity
        .and_then(|p| mpd.tracks.get(&p).cloned())
        .unwrap_or_default();

    let meta_json = meta_to_json(
        &meta.map,
        &meta.client,
        &meta.server,
        &meta.game,
        meta.protocol,
        meta.duration,
        meta.ncmds,
        meta.tick_rate,
        0.0,
    );

    let mut html = HTML_TEMPLATE.to_string();
    html = html.replace("__DEMO_NAME__", &escape_html(name_hint));
    html = html.replace("__META__", &meta_json);
    html = html.replace("__CMDS__", "[]");
    html = html.replace("__LIFE_BREAKS__", "[]");
    html = html.replace("__TELEPORT_BREAKS__", "[]");
    html = html.replace("__EVENTS__", "[]");
    html = html.replace("__WORLD_POSITIONS__", &world_positions_to_json(&world_positions));
    // Quake map overlay (Q1 / Q2 / Q3 BSP), if a matching .bsp was found.
    let (q_verts, q_idx, q_spawn) = match bsp_bytes {
        Some(bytes) => match extract_any_bsp(bytes) {
            Some((v, i, nv, nt, spawn)) => {
                eprintln!("  Quake BSP: {} verts, {} tris", nv, nt);
                (v, i, spawn)
            }
            None => {
                eprintln!("  Quake BSP extraction failed (unsupported version?)");
                (String::new(), String::new(), [0.0f32; 3])
            }
        },
        None => (String::new(), String::new(), [0.0f32; 3]),
    };
    html = html.replace("__BSP_VERTS__", &format!("\"{}\"", q_verts));
    html = html.replace("__BSP_IDX__", &format!("\"{}\"", q_idx));
    html = html.replace("__BSP_SPAWN__", &spawn_to_json(q_spawn));
    html = html.replace("__ENTITY_TRACKS__", &multi_tracks_to_json(mpd));
    html = html.replace("__ENTITY_NAMES__", &multi_names_to_json(mpd));
    html = html.replace("__ENTITY_LIFE_STATES__", &multi_life_states_to_json(mpd));
    html = html.replace("__ENTITY_OBSERVER__", &multi_observer_modes_to_json(mpd));
    html = html.replace("__ENTITY_YAWS__", &multi_yaws_to_json(mpd));
    html = html.replace("__ENTITY_WEAPONS__", &multi_weapons_to_json(mpd));
    html = html.replace("__WEAPON_CLASSES__", &multi_weapon_classes_to_json(mpd));
    html = html.replace("__ENTITY_BUTTONS__", "{}"); // Quake demos carry no button state
    html = html.replace(
        "__PRIMARY_ENTITY__",
        &mpd.primary_entity.map(|e| e.to_string()).unwrap_or_else(|| "null".to_string()),
    );
    html = html.replace("__VIEW_ANGLES__", &view_angles_to_json(mpd));
    html = html.replace("__VIEW_SWITCHES__", "[]");
    html = html.replace("__GHOST_EIDS__", "[]");
    Ok(html)
}

// Source 2 (`PBDEMS2`: CS2 / Dota 2 / Deadlock) HTML generator — metadata-only.
//
// The full entity-decode pipeline (FlattenedSerializer + field-path Huffman +
// PacketEntities) isn't built yet, so there are no position tracks. What IS
// decoded today: the container header, `CDemoFileHeader` (map / server / build)
// and `CDemoFileInfo` (duration / ticks). We fill the SAME template so a Source
// 2 demo opens to a valid viewer showing its metadata instead of being rejected
// on magic — every track/event section is empty until the entity stages land.
pub fn generate_source2_html(
    data: &[u8],
    vpk_bytes: Option<&[u8]>,
    dem_path: Option<&Path>,
    name_hint: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let meta = source2::parse_meta(data)
        .ok_or_else(|| Box::<dyn std::error::Error>::from("not a valid PBDEMS2 (Source 2) file"))?;
    eprintln!(
        "  Source 2 demo: map={} server={} build={} demo_version={} duration={:.1}s ticks={}",
        meta.map_name, meta.server_name, meta.build_num, meta.demo_version_name,
        meta.playback_time, meta.playback_ticks,
    );

    let tick_rate = if meta.playback_time > 0.0 && meta.playback_ticks > 0 {
        meta.playback_ticks as f32 / meta.playback_time
    } else {
        64.0 // CS2 default
    };

    // Decode per-pawn entity tracks (FlattenedSerializer + field-path Huffman +
    // PacketEntities). May be empty if decode fails on an unsupported build.
    let tracks = source2::parse(data).unwrap_or_default();
    let total: usize = tracks.tracks.values().map(|v| v.len()).sum();
    let named = tracks.names.len();
    let deaths: usize = tracks.life_states.values().flatten().filter(|(_, s)| *s != 0).count();
    eprintln!(
        "  Source 2 entities: {} player tracks ({} named), {} samples ({} packets ok / {} failed)",
        tracks.tracks.len(), named, total, tracks.pe_ok, tracks.pe_fail
    );
    let kills = tracks.events.iter().filter(|e| e.event == "player_death").count();
    let bombs = tracks.events.iter().filter(|e| e.event.starts_with("bomb_")).count();
    eprintln!(
        "  Source 2 gameplay: {} events ({} kills, {} bomb), {} death transitions, {} rounds",
        tracks.events.len(), kills, bombs, deaths,
        tracks.rounds.last().map(|(_, r)| *r).unwrap_or(0),
    );
    for econ in tracks.econ.values() {
        eprintln!("    {} — {}K/{}D/{}A, ${}, team {}", econ.name, econ.kills, econ.deaths, econ.assists, econ.money, econ.team);
    }
    // Prefer the map name recovered from svc_ServerInfo (the header is often blank
    // on loopback recordings).
    let map_name = if !tracks.map_name.is_empty() { tracks.map_name.clone() } else { meta.map_name.clone() };

    let has_track = !tracks.tracks.is_empty();
    let mut eids: Vec<i32> = tracks.tracks.keys().copied().collect();
    eids.sort_unstable();

    // Per-tick dedup keeps the viewer's tick-indexed lookups monotonic.
    let mut track_parts = Vec::new();
    let mut yaw_parts = Vec::new();
    let mut name_parts = Vec::new();
    for eid in &eids {
        let mut last = i32::MIN;
        let mut pts = Vec::new();
        for (tk, x, y, z) in &tracks.tracks[eid] {
            if *tk <= last { continue; }
            last = *tk;
            pts.push(format!("[{},{},{},{}]", tk, json_f32(*x), json_f32(*y), json_f32(*z)));
        }
        track_parts.push(format!("\"{}\":[{}]", eid, pts.join(",")));

        if let Some(ys) = tracks.yaws.get(eid) {
            let mut last = i32::MIN;
            let mut yp = Vec::new();
            for (tk, yaw, pitch) in ys {
                if *tk <= last { continue; }
                last = *tk;
                yp.push(format!("[{},{:.1},{:.1}]", tk, yaw, pitch));
            }
            yaw_parts.push(format!("\"{}\":[{}]", eid, yp.join(",")));
        }

        let nm = tracks.names.get(eid).cloned().unwrap_or_else(|| format!("player {}", eid));
        // Attach the player's latest scoreboard/economy so the sidebar can show it.
        let (k, d, a, money, score, team) = tracks.econ_by_pawn.get(eid)
            .map(|e| (e.kills, e.deaths, e.assists, e.money, e.score, e.team))
            .unwrap_or((0, 0, 0, 0, 0, 0));
        name_parts.push(format!(
            "\"{}\":{{\"name\":\"{}\",\"steam_id\":\"\",\"user_id\":{},\"is_fake\":false,\"is_hltv\":false,\"aliases\":[\"{}\"],\"kills\":{},\"deaths\":{},\"assists\":{},\"money\":{},\"score\":{},\"team\":{}}}",
            eid, escape_json_str(&nm), eid, escape_json_str(&nm), k, d, a, money, score, team,
        ));
    }
    let entity_tracks_json = format!("{{{}}}", track_parts.join(","));
    let entity_yaws_json = format!("{{{}}}", yaw_parts.join(","));
    let entity_names_json = format!("{{{}}}", name_parts.join(","));
    // Highlight the recording player's track: match the demo header's client_name
    // (the recorder, e.g. a POV demo's local player) to a tracked pawn's name.
    // GOTV/SourceTV demos have no player recorder, so fall back to the first eid.
    let recorder = meta.client_name.trim();
    let primary_json = eids.iter()
        .find(|e| tracks.names.get(e).map(|n| n.as_str()) == Some(recorder))
        .or_else(|| eids.first())
        .map(|e| e.to_string())
        .unwrap_or_else(|| "null".to_string());

    // Per-pawn life-state transitions drive the viewer's death-hiding (avatar
    // hidden while m_lifeState != 0). Shape: { eid: [[tick, state], ...] }.
    let life_parts: Vec<String> = eids
        .iter()
        .filter_map(|eid| tracks.life_states.get(eid).map(|st| (eid, st)))
        .map(|(eid, st)| {
            let pts: Vec<String> = st.iter().map(|(t, s)| format!("[{},{}]", t, s)).collect();
            format!("\"{}\":[{}]", eid, pts.join(","))
        })
        .collect();
    let entity_life_states_json = format!("{{{}}}", life_parts.join(","));

    // Decoded gameplay events (kills, bomb, grenades) feed the event timeline.
    // CS2 does NOT emit round_end / round_start as legacy game events (only beeps
    // + round_freeze_end), so the only reliable round boundary is the
    // m_totalRoundsPlayed transition stream. Synthesize a `round_end` marker per
    // completed round from it and merge into the event list — this is what makes
    // "round end" show up for CS2 (CS:GO/CS:S carry the real events via Source 1).
    let round_markers: Vec<String> = tracks.rounds.iter()
        .filter(|(_, n)| *n > 0)
        .map(|(tick, n)| format!("{{\"event\":\"round_end\",\"tick\":{},\"round\":{}}}", tick, n))
        .collect();
    let events_json = {
        let real = events_to_json(&tracks.events);
        let inner = real.trim().trim_start_matches('[').trim_end_matches(']').trim();
        let mut parts: Vec<String> = Vec::new();
        if !inner.is_empty() { parts.push(inner.to_string()); }
        if !round_markers.is_empty() { parts.push(round_markers.join(",")); }
        format!("[{}]", parts.join(","))
    };

    let meta_json = meta_to_json(
        &map_name,
        &meta.client_name,
        &meta.server_name,
        &meta.game_directory,
        meta.network_protocol,
        meta.playback_time,
        meta.playback_frames.max(0) as usize,
        tick_rate,
        0.0,
    );

    let mut html = HTML_TEMPLATE.to_string();
    html = html.replace("__DEMO_NAME__", &escape_html(name_hint));
    html = html.replace("__META__", &meta_json);
    html = html.replace("__CMDS__", "[]");
    html = html.replace("__LIFE_BREAKS__", "[]");
    html = html.replace("__TELEPORT_BREAKS__", "[]");
    html = html.replace("__EVENTS__", &events_json);
    html = html.replace("__WORLD_POSITIONS__", &world_positions_to_json(&[]));
    // CS2 map overlay: the world collision mesh pulled from the matching `.vpk`
    // (Source 2 maps aren't VBSP). Same base64 vert/index format the BSP path
    // uses, so the viewer renders it with no template change. The map name is
    // only known now (it comes from svc_ServerInfo, blank in the header on
    // loopback recordings), so the on-disk pak is resolved here — the explicit
    // `vpk_bytes` (the browser drag-drop buffer) takes priority.
    let resolved_vpk: Option<Vec<u8>> = if vpk_bytes.is_some() {
        None
    } else {
        dem_path
            .filter(|_| !map_name.is_empty())
            .and_then(|p| find_vpk_file(p, &map_name))
            .and_then(|p| {
                eprintln!("  Found map pak: {}", p.file_name().unwrap_or_default().to_string_lossy());
                std::fs::read(&p).ok()
            })
    };
    let vpk = vpk_bytes.or(resolved_vpk.as_deref());
    let (s2_verts, s2_idx) = match vpk.and_then(source2::map::extract_map_geometry) {
        Some((v, i, nv, nt)) => {
            eprintln!("  Source 2 map: {} verts, {} triangles from .vpk world_physics", nv, nt);
            (v, i)
        }
        None => {
            if vpk_bytes.is_some() {
                eprintln!("  Source 2 map: .vpk present but no world geometry decoded");
            }
            (String::new(), String::new())
        }
    };
    html = html.replace("__BSP_VERTS__", &format!("\"{}\"", s2_verts));
    html = html.replace("__BSP_IDX__", &format!("\"{}\"", s2_idx));
    html = html.replace("__BSP_SPAWN__", &spawn_to_json([0.0f32; 3]));
    html = html.replace("__ENTITY_TRACKS__", if has_track { &entity_tracks_json } else { "{}" });
    html = html.replace("__ENTITY_NAMES__", if has_track { &entity_names_json } else { "{}" });
    html = html.replace("__ENTITY_LIFE_STATES__", if has_track { &entity_life_states_json } else { "{}" });
    html = html.replace("__ENTITY_OBSERVER__", "{}");
    html = html.replace("__ENTITY_YAWS__", if has_track { &entity_yaws_json } else { "{}" });
    // Active-weapon streams (CS2; empty for Deadlock/Dota, which have no held
    // weapon). Shape mirrors the Source 1 path: ENTITY_WEAPONS { pawn: [[tick,
    // weaponClassId], …] } resolved through WEAPON_CLASSES { weaponClassId:
    // "ak47" } by the viewer's weaponAt().
    let weapons_json = {
        let parts: Vec<String> = tracks.weapons.iter()
            .filter(|(_, sw)| !sw.is_empty())
            .map(|(eid, sw)| {
                let pts: Vec<String> = sw.iter().map(|(t, c)| format!("[{},{}]", t, c)).collect();
                format!("\"{}\":[{}]", eid, pts.join(","))
            })
            .collect();
        format!("{{{}}}", parts.join(","))
    };
    let weapon_classes_json = {
        let parts: Vec<String> = tracks.weapon_names.iter()
            .filter(|(_, n)| !n.is_empty())
            .map(|(cid, n)| format!("\"{}\":\"{}\"", cid, escape_json_str(n)))
            .collect();
        format!("{{{}}}", parts.join(","))
    };
    html = html.replace("__ENTITY_WEAPONS__", if has_track { &weapons_json } else { "{}" });
    html = html.replace("__WEAPON_CLASSES__", if has_track { &weapon_classes_json } else { "{}" });
    // Real per-pawn input streams (CS2 / Deadlock / Dota): { pawn: [[tick, mask], …] }
    // decoded from the movement-services button state — the viewer lights actual
    // W/A/S/D/attack/jump/duck from these instead of inferring from velocity.
    let buttons_json = {
        let parts: Vec<String> = tracks.buttons.iter()
            .filter(|(_, b)| !b.is_empty())
            .map(|(eid, b)| {
                let pts: Vec<String> = b.iter().map(|(t, m)| format!("[{},{}]", t, m)).collect();
                format!("\"{}\":[{}]", eid, pts.join(","))
            })
            .collect();
        format!("{{{}}}", parts.join(","))
    };
    html = html.replace("__ENTITY_BUTTONS__", if has_track { &buttons_json } else { "{}" });
    html = html.replace("__PRIMARY_ENTITY__", if has_track { &primary_json } else { "null" });
    html = html.replace("__VIEW_ANGLES__", "[]");
    html = html.replace("__VIEW_SWITCHES__", "[]");
    html = html.replace("__GHOST_EIDS__", "[]");
    Ok(html)
}

// GoldSrc (HL1) HTML generator: decodes the HLDEMO container + the recorder
// camera path (eye origin + view angles from the NetMsg RefParams stream) and
// fills the SAME template - the recorder renders as a single entity track so the
// route / follow / FPS camera all work. The matching GoldSrc `.bsp` (v30), if
// supplied, is overlaid via `extract_goldsrc_bsp_from_bytes`.
pub fn generate_goldsrc_html(
    data: &[u8],
    bsp_bytes: Option<&[u8]>,
    name_hint: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let meta = goldsrc::parse(data)
        .ok_or_else(|| Box::<dyn std::error::Error>::from("not a valid HLDEMO (GoldSrc) file"))?;
    eprintln!(
        "  GoldSrc demo: map={} game={} demo_proto={} net_proto={} duration={:.1}s frames={}",
        meta.map_name, meta.game_dir, meta.demo_protocol, meta.net_protocol, meta.duration, meta.frame_count
    );

    let tick_rate = if meta.duration > 0.0 && meta.frame_count > 0 {
        meta.frame_count as f32 / meta.duration
    } else {
        100.0
    };

    // Decode the recorder camera path (eye origin + view angles) from the
    // NetMsg RefParams stream, mapped onto a synthetic tick = time × tick_rate.
    let cam = goldsrc::extract_camera(data, &meta);
    let mut world_positions: Vec<(i32, f32, f32, f32)> = Vec::with_capacity(cam.len());
    let mut view_angles: Vec<(i32, f32, f32)> = Vec::with_capacity(cam.len());
    let mut yaws: Vec<(i32, f32, f32)> = Vec::with_capacity(cam.len()); // (tick, yaw, pitch)
    let mut last_tick = i32::MIN;
    for &(time, x, y, z, pitch, yaw) in &cam {
        let tick = (time * tick_rate).round() as i32;
        // NetMsg samples are time-ordered; keep one per tick so the viewer's
        // tick-indexed lookups stay monotonic.
        if tick <= last_tick {
            continue;
        }
        last_tick = tick;
        world_positions.push((tick, x, y, z));
        view_angles.push((tick, pitch, yaw));
        yaws.push((tick, yaw, pitch));
    }
    eprintln!("  GoldSrc camera: {} samples ({} after per-tick dedup)", cam.len(), world_positions.len());

    // Feed the recorder as a single multi-player entity (id 1) - the same shape
    // the Quake path uses. GoldSrc demos carry no usercmds, so `CMDS` is empty
    // and the WORLD_POSITIONS→finalPositions interpolation (which is indexed by
    // CMDS ticks) would yield nothing; rendering the recorder as a real entity
    // track with its own samples sidesteps that and drives the avatar, route,
    // minimap, and follow/FPS camera directly.
    const REC_EID: i32 = 1;
    let rec_name = if meta.game_dir.is_empty() { "recorder".to_string() } else { format!("{} recorder", meta.game_dir) };
    let view_angles_json = {
        // VIEW_ANGLES shape is [tick, pitch, yaw].
        let parts: Vec<String> = view_angles.iter().map(|(t, p, y)| format!("[{},{:.2},{:.2}]", t, p, y)).collect();
        format!("[{}]", parts.join(","))
    };

    // Decode the *other* players out of the svc entity stream (delta-compressed
    // PacketEntities). When that yields tracks we render real per-player dots;
    // otherwise we fall back to driving a single avatar from the recorder camera.
    let mut ents = goldsrc::extract_entities(data, &meta);
    // Include the recorder itself as a named, followable entity, sourced from
    // the camera path (its own player entity isn't in PacketEntities - the local
    // player is networked via svc_clientdata). Its name comes from slot-0 userinfo
    // if present, else the generic recorder label.
    if !ents.tracks.is_empty() {
        ents.tracks
            .entry(REC_EID as u32)
            .or_insert_with(|| cam.iter().map(|&(t, x, y, z, _, _)| (t, x, y, z)).collect());
        ents.yaws
            .entry(REC_EID as u32)
            .or_insert_with(|| cam.iter().map(|&(t, _, _, _, p, y)| (t, y, p)).collect());
        ents.names.entry(REC_EID as u32).or_insert_with(|| rec_name.clone());
        ents.primary = Some(REC_EID as u32);
    }
    let to_tick = |time: f32| (time * tick_rate).round() as i32;
    let entity_tracks_json;
    let entity_names_json;
    let entity_yaws_json;
    let primary_json: String;
    let has_track;
    if !ents.tracks.is_empty() {
        let mut eids: Vec<u32> = ents.tracks.keys().copied().collect();
        eids.sort_unstable();
        let mut track_parts = Vec::with_capacity(eids.len());
        let mut yaw_parts = Vec::with_capacity(eids.len());
        let mut name_parts = Vec::with_capacity(eids.len());
        let mut total_samples = 0usize;
        for eid in &eids {
            // Per-tick dedup keeps the viewer's tick-indexed lookups monotonic.
            let mut last = i32::MIN;
            let mut pts = Vec::new();
            for (time, x, y, z) in &ents.tracks[eid] {
                let tk = to_tick(*time);
                if tk <= last { continue; }
                last = tk;
                pts.push(format!("[{},{},{},{}]", tk, json_f32(*x), json_f32(*y), json_f32(*z)));
            }
            total_samples += pts.len();
            track_parts.push(format!("\"{}\":[{}]", eid, pts.join(",")));
            if let Some(ys) = ents.yaws.get(eid) {
                let mut last = i32::MIN;
                let mut yp = Vec::new();
                for (time, yaw, pitch) in ys {
                    let tk = to_tick(*time);
                    if tk <= last { continue; }
                    last = tk;
                    yp.push(format!("[{},{:.1},{:.1}]", tk, yaw, pitch));
                }
                yaw_parts.push(format!("\"{}\":[{}]", eid, yp.join(",")));
            }
            let nm = ents.names.get(eid).cloned().unwrap_or_else(|| format!("player {}", eid));
            name_parts.push(format!(
                "\"{}\":{{\"name\":\"{}\",\"steam_id\":\"\",\"user_id\":{},\"is_fake\":false,\"is_hltv\":false,\"aliases\":[\"{}\"]}}",
                eid, escape_json_str(&nm), eid, escape_json_str(&nm),
            ));
        }
        entity_tracks_json = format!("{{{}}}", track_parts.join(","));
        entity_yaws_json = format!("{{{}}}", yaw_parts.join(","));
        entity_names_json = format!("{{{}}}", name_parts.join(","));
        primary_json = ents.primary.or_else(|| eids.first().copied()).unwrap().to_string();
        has_track = true;
        eprintln!("  GoldSrc entities: {} player tracks, {} samples", eids.len(), total_samples);
    } else {
        // Recorder-only fallback: render the recorder camera path as entity 1.
        let pts: Vec<String> = world_positions
            .iter()
            .map(|(t, x, y, z)| format!("[{},{},{},{}]", t, json_f32(*x), json_f32(*y), json_f32(*z)))
            .collect();
        entity_tracks_json = format!("{{\"{}\":[{}]}}", REC_EID, pts.join(","));
        entity_names_json = format!(
            "{{\"{}\":{{\"name\":\"{}\",\"steam_id\":\"\",\"user_id\":{},\"is_fake\":false,\"is_hltv\":false,\"aliases\":[\"{}\"]}}}}",
            REC_EID, escape_json_str(&rec_name), REC_EID, escape_json_str(&rec_name),
        );
        let yp: Vec<String> = yaws.iter().map(|(t, y, p)| format!("[{},{:.1},{:.1}]", t, y, p)).collect();
        entity_yaws_json = format!("{{\"{}\":[{}]}}", REC_EID, yp.join(","));
        primary_json = REC_EID.to_string();
        has_track = !world_positions.is_empty();
    }

    let meta_json = meta_to_json(
        &meta.map_name,
        &rec_name,
        "",               // no server name field
        &meta.game_dir,
        meta.demo_protocol,
        meta.duration,
        world_positions.len(),
        tick_rate,
        0.0,
    );

    let mut html = HTML_TEMPLATE.to_string();
    html = html.replace("__DEMO_NAME__", &escape_html(name_hint));
    html = html.replace("__META__", &meta_json);
    html = html.replace("__CMDS__", "[]");
    html = html.replace("__LIFE_BREAKS__", "[]");
    html = html.replace("__TELEPORT_BREAKS__", "[]");
    html = html.replace("__EVENTS__", "[]");
    html = html.replace("__WORLD_POSITIONS__", &world_positions_to_json(&world_positions));
    // GoldSrc map geometry (BSP v30 / Q1 v29), if a matching .bsp was supplied.
    let (bsp_verts_b64, bsp_idx_b64, bsp_spawn) = match bsp_bytes {
        Some(bytes) => match extract_goldsrc_bsp_from_bytes(bytes) {
            Some((v, i, nv, nt, spawn)) => {
                eprintln!("  GoldSrc BSP: {} verts, {} tris, spawn=[{:.1},{:.1},{:.1}]", nv, nt, spawn[0], spawn[1], spawn[2]);
                (v, i, spawn)
            }
            None => {
                eprintln!("  GoldSrc BSP extraction failed");
                (String::new(), String::new(), [0.0f32; 3])
            }
        },
        None => (String::new(), String::new(), [0.0f32; 3]),
    };
    html = html.replace("__BSP_VERTS__", &format!("\"{}\"", bsp_verts_b64));
    html = html.replace("__BSP_IDX__", &format!("\"{}\"", bsp_idx_b64));
    html = html.replace("__BSP_SPAWN__", &spawn_to_json(bsp_spawn));
    html = html.replace("__ENTITY_TRACKS__", if has_track { &entity_tracks_json } else { "{}" });
    html = html.replace("__ENTITY_NAMES__", if has_track { &entity_names_json } else { "{}" });
    html = html.replace("__ENTITY_LIFE_STATES__", "{}");
    html = html.replace("__ENTITY_OBSERVER__", "{}");
    html = html.replace("__ENTITY_YAWS__", if has_track { &entity_yaws_json } else { "{}" });
    html = html.replace("__ENTITY_WEAPONS__", "{}");
    html = html.replace("__WEAPON_CLASSES__", "{}");
    html = html.replace("__ENTITY_BUTTONS__", "{}"); // GoldSrc: no networked button stream decoded
    html = html.replace("__PRIMARY_ENTITY__", if has_track { &primary_json } else { "null" });
    html = html.replace("__VIEW_ANGLES__", &view_angles_json);
    html = html.replace("__VIEW_SWITCHES__", "[]");
    html = html.replace("__GHOST_EIDS__", "[]");
    Ok(html)
}
