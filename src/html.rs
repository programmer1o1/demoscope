// HTML viewer assembly: the three entry points that turn demo bytes into the
// self-contained viewer. `generate_html` is the CLI file wrapper, then it
// (and the wasm shim) call the pure-bytes `generate_html_string` for Source
// demos or `generate_quake_html` for the Quake family. The shared template is
// `include_str!`'d here and the parsed model is spliced in via the `json`
// serializers.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write as IoWrite};
use std::path::Path;

use super::bytes::{le_f32, le_i32};
use super::constants::{HEADER_SIZE, SPLIT_SIZE};
use super::events::{
    compute_life_breaks, display_events_for_game, extract_events_from_payload,
    scan_for_game_event_list, EventField, EventValue, GameEvent, SampledCmd,
};
use super::bsp::{extract_bsp_from_bytes, find_bsp_file};
use super::header::{parse_header, parse_usercmd};
use super::json::{
    breaks_to_json, cmds_to_json, escape_html, escape_json_str, events_to_json, json_f32,
    meta_to_json, multi_life_states_to_json, multi_names_to_json, multi_observer_modes_to_json,
    multi_tracks_to_json, multi_weapon_classes_to_json, multi_weapons_to_json, multi_yaws_to_json,
    spawn_to_json, view_angles_to_json, world_positions_to_json,
};
use super::packets::{
    detect_splitscreen, extract_svc_setview, iterate_demo_packets, parse_userinfo_from_demo,
    spectator_switch_intervals,
};
use super::{goldsrc, multi_player, quake, source_demo};

const HTML_TEMPLATE: &str = include_str!("template.html");

const MAX_CMDS_EMBED: usize = 20_000;

// CLI wrapper - reads the dem + (optional) bsp from disk, delegates to the
// pure-bytes core, then writes the result. Keeps the existing command-line
// behaviour intact while the WASM build calls the core directly.
pub(crate) fn generate_html(dem_path: &Path, output_path: &Path, jump_threshold: f32) -> io::Result<()> {
    eprintln!("Reading {} ...", dem_path.file_name().unwrap_or_default().to_string_lossy());
    let mut file = File::open(dem_path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    // Quake-family demos (Q1/Q2/Q3) are a different format from HL2DEMO; route
    // them to the dedicated decoder, which emits the same HTML viewer.
    let name_hint = dem_path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if let Some(kind) = quake::detect(&name_hint, &data) {
        // Resolve a matching .bsp beside the demo. The map name can arrive as a
        // bare stem or a `maps/foo.bsp` path, so normalise to the file stem.
        let bsp_bytes: Option<Vec<u8>> = quake::parse(kind, &data, &name_hint)
            .ok()
            .and_then(|demo| {
                let stem = Path::new(&demo.meta.map)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or(demo.meta.map);
                find_bsp_file(dem_path, &stem)
            })
            .and_then(|p| {
                eprintln!("  Found BSP: {}", p.file_name().unwrap_or_default().to_string_lossy());
                std::fs::read(&p).ok()
            });
        let html = generate_quake_html(&data, kind, bsp_bytes.as_deref(), &name_hint)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let mut out_file = File::create(output_path)?;
        out_file.write_all(html.as_bytes())?;
        eprintln!("HTML -> {}  ({:.1} KB)", output_path.display(), html.len() as f64 / 1024.0);
        return Ok(());
    }

    // GoldSrc (Half-Life 1 / CS 1.6 / DoD / CZ) HLDEMO container - recorder POV
    // + (if the map is alongside) the BSP overlay. Routed before HL2DEMO parse.
    if goldsrc::is_goldsrc(&data) {
        let bsp_bytes: Option<Vec<u8>> = goldsrc::parse(&data)
            .and_then(|m| find_bsp_file(dem_path, &m.map_name))
            .and_then(|p| {
                eprintln!("  Found BSP: {}", p.file_name().unwrap_or_default().to_string_lossy());
                std::fs::read(&p).ok()
            });
        let html = generate_goldsrc_html(&data, bsp_bytes.as_deref(), &name_hint)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let mut out_file = File::create(output_path)?;
        out_file.write_all(html.as_bytes())?;
        eprintln!("HTML -> {}  ({:.1} KB)", output_path.display(), html.len() as f64 / 1024.0);
        return Ok(());
    }

    // Resolve a BSP file alongside the demo and read it once here, so the
    // core never needs to touch the filesystem.
    let header = parse_header(&data).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "not a valid HL2DEMO file")
    })?;
    let bsp_bytes: Option<Vec<u8>> = match find_bsp_file(dem_path, &header.map_name) {
        Some(p) => {
            eprintln!("  Found BSP: {}", p.file_name().unwrap_or_default().to_string_lossy());
            let mut f = File::open(&p)?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            Some(buf)
        }
        None => {
            eprintln!("  No BSP found alongside demo");
            None
        }
    };
    let html = generate_html_string(&data, bsp_bytes.as_deref(), &name_hint, jump_threshold)?;
    let mut out_file = File::create(output_path)?;
    out_file.write_all(html.as_bytes())?;
    let size_kb = html.len() as f64 / 1024.0;
    eprintln!("HTML -> {}  ({:.1} KB)", output_path.display(), size_kb);
    Ok(())
}

// Pure-bytes core: takes the demo + optional BSP as byte slices, returns the
// generated HTML as a String. No filesystem access - used by both the CLI
// wrapper above and the WASM entry point in lib.rs.
pub fn generate_html_string(
    data: &[u8],
    bsp_bytes: Option<&[u8]>,
    name_hint: &str,
    jump_threshold: f32,
) -> io::Result<String> {
    let header = parse_header(data).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "not a valid HL2DEMO file")
    })?;

    // Compute tick rate from header (ticks / playback_time), fallback to 66 Hz
    let tick_rate = if header.playback_time > 0.0 && header.ticks > 0 {
        header.ticks as f32 / header.playback_time
    } else {
        66.0_f32
    };

    // Collect usercmds (SampledCmd) and all packet info
    let mut all_cmds: Vec<SampledCmd> = Vec::new();
    let mut last_pitch = 0.0f32;
    let mut last_yaw = 0.0f32;
    let mut last_weapon: u32 = 0;
    let mut console_cmds: Vec<(i32, String)> = Vec::new(); // (tick, cmd_string)
    let mut world_positions: Vec<(i32, f32, f32, f32)> = Vec::new();

    // We need to iterate packets twice: once for usercmds, once for events
    // So let's do it in one pass
    let mut signon_payloads: Vec<Vec<u8>> = Vec::new();
    let mut game_packet_ticks: Vec<(i32, usize, usize)> = Vec::new(); // (tick, start, end)

    let proto = header.demo_protocol;
    let extra: usize = if proto > 3 { 1 } else { 0 }; // player_slot byte in proto > 3
    let packets = iterate_demo_packets(&data, proto, &header.game_dir);

    // Collect signon payloads and game packet locations
    for pkt in &packets {
        match pkt.cmd {
            1 => {
                if pkt.payload_end > pkt.payload_start {
                    signon_payloads.push(data[pkt.payload_start..pkt.payload_end].to_vec());
                }
            }
            2 => {
                if pkt.payload_end > pkt.payload_start {
                    game_packet_ticks.push((pkt.tick, pkt.payload_start, pkt.payload_end));
                }
            }
            _ => {}
        }
    }

    // Parse usercmds
    // net_protocol <= 7 (2004-era HL2/GMod9 engine) uses WriteBitAngle(16) for
    // viewangles instead of WriteFloat(32) - our parser would produce NaN/garbage.
    // Skip usercmd extraction for those demos; header/summary still work fine.
    let usercmd_supported = header.net_protocol > 7;
    if !usercmd_supported {
        eprintln!("  Note: net_protocol={} (old engine) - usercmd format unsupported, skipping input parsing", header.net_protocol);
    }

    if usercmd_supported {
        let pkt_hdr = 5 + extra; // cmd(1)+tick(4)+[slot(1)]
        let democmdinfo = SPLIT_SIZE * detect_splitscreen(&data, proto, &header.game_dir); // L4D = 4 slots
        let mut offset = HEADER_SIZE;
        while offset < data.len() {
            if offset + 5 > data.len() { break; }
            let cmd = data[offset];
            let tick = le_i32(&data, offset + 1);
            match cmd {
                7 => break,
                1 | 2 => {
                    let p = offset + pkt_hdr;
                    if p + democmdinfo + 12 > data.len() { break; }
                    // democmdinfo layout: flags(4) + viewOrigin(12) + ...
                    // Extract viewOrigin from cmd=2 (game packets only, not signon)
                    if cmd == 2 && p + 16 <= data.len() {
                        let x = le_f32(&data, p + 4);
                        let y = le_f32(&data, p + 8);
                        let z = le_f32(&data, p + 12);
                        if x != 0.0 || y != 0.0 || z != 0.0 {
                            world_positions.push((tick, x, y, z));
                        }
                    }
                    let length = le_i32(&data, p + democmdinfo + 8);
                    if length < 0 { break; }
                    offset = p.saturating_add(democmdinfo + 12).saturating_add(length as usize);
                }
                3 => { offset += pkt_hdr; }
                4 => {
                    let p = offset + pkt_hdr;
                    if p + 4 > data.len() { break; }
                    let length = le_i32(&data, p);
                    if length < 0 { break; }
                    let length = length as usize;
                    if p + 4 + length <= data.len() {
                        let s = std::str::from_utf8(&data[p + 4..p + 4 + length])
                            .unwrap_or("")
                            .trim_matches('\0')
                            .trim()
                            .to_string();
                        if !s.is_empty() {
                            console_cmds.push((tick, s));
                        }
                    }
                    offset = p.saturating_add(4).saturating_add(length);
                }
                5 => {
                    let p = offset + pkt_hdr;
                    if p + 8 > data.len() { break; }
                    let out_seq = le_i32(&data, p);
                    let length = le_i32(&data, p + 4);
                    if length < 0 { break; }
                    let next = p.saturating_add(8).saturating_add(length as usize);
                    if next > data.len() { break; }
                    let ucmd_bytes = &data[p + 8..next];
                    if let Some(ucmd) = parse_usercmd(ucmd_bytes) {
                        let cmd_num = ucmd.command_number.unwrap_or(out_seq as u32);
                        let _ = cmd_num;
                        if let Some(p) = ucmd.pitch { last_pitch = p; }
                        if let Some(y) = ucmd.yaw { last_yaw = y; }
                        if let Some(w) = ucmd.weaponselect { last_weapon = w; }
                        all_cmds.push(SampledCmd {
                            tick,
                            pitch: last_pitch,
                            yaw: last_yaw,
                            fwd: ucmd.forwardmove.unwrap_or(0.0),
                            side: ucmd.sidemove.unwrap_or(0.0),
                            btns: ucmd.buttons.unwrap_or(0),
                            weapon: last_weapon,
                        });
                    }
                    offset = next;
                }
                // 6=DataTables, 8=StringTables(old enum)/CustomData(new enum),
                // 9=StringTables(new enum, L4D/CS:GO). All length-prefixed, so
                // we can skip them uniformly. (CustomData is callback+length, but
                // POV demos don't carry it - see detect_splitscreen note.)
                6 | 8 | 9 => {
                    let p = offset + pkt_hdr;
                    if p + 4 > data.len() { break; }
                    let length = le_i32(&data, p);
                    if length < 0 { break; }
                    offset = p.saturating_add(4).saturating_add(length as usize).min(data.len());
                }
                _ => break,
            }
        }
    } // end if usercmd_supported

    eprintln!(
        "  map={}  client={}  duration={:.1}s  rows={}",
        header.map_name, header.client_name, header.playback_time, all_cmds.len()
    );

    // Parse game events
    eprint!("  Parsing game events ...");
    io::stderr().flush().ok();

    let display_ev = display_events_for_game(&header.game_dir);
    // Proto-4 net-message remap flags (same derivation as player_tracks): L4D
    // shares the Portal 2 engine's renumbering but keeps the 11-bit user-message
    // width, so the two axes are independent.
    let p2_engine = source_demo::datatable::is_portal2_engine(&header.game_dir);
    let l4d_msgmap = matches!(header.game_dir.as_str(), "left4dead" | "left4dead2");
    let remap_msgs = p2_engine || l4d_msgmap;
    let user_msg_12bit = p2_engine;
    let mut game_events: Vec<GameEvent> = Vec::new();

    let schemas = {
        let mut found = None;
        for payload in &signon_payloads {
            if let Some(s) = scan_for_game_event_list(payload) {
                found = Some(s);
                break;
            }
        }
        found
    };

    // Fallback: if signon had no event list, check the first 20 game packets
    let schemas = if schemas.is_none() {
        let mut found = None;
        for &(_, start, end) in game_packet_ticks.iter().take(20) {
            if let Some(s) = scan_for_game_event_list(&data[start..end]) {
                found = Some(s);
                break;
            }
        }
        found
    } else {
        schemas
    };

    if let Some(ref schemas) = schemas {
        for &(tick, start, end) in &game_packet_ticks {
            let payload = &data[start..end];
            let evs = extract_events_from_payload(payload, tick, schemas, &display_ev, remap_msgs, user_msg_12bit, header.demo_protocol);
            game_events.extend(evs);
        }
    }

    // Convert collected console commands into GameEvent entries
    if display_ev.contains("console_cmd") {
        for (tick, cmd_str) in &console_cmds {
            game_events.push(GameEvent {
                event: "console_cmd".to_string(),
                tick: *tick,
                fields: vec![EventField {
                    name: "cmd".to_string(),
                    value: EventValue::Str(cmd_str.clone()),
                }],
            });
        }
        game_events.sort_by_key(|e| e.tick);
    }

    eprintln!(" {} found", game_events.len());

    // Parse player info from DEM_STRINGTABLES
    let (userinfo, _slot_to_uid) = parse_userinfo_from_demo(&data, header.demo_protocol, &header.game_dir);
    eprintln!("  Players: {}", userinfo.len());

    // BSP - caller supplies the bytes (or None). The pure-bytes core never
    // touches the filesystem; the CLI wrapper handles file resolution.
    let (bsp_verts_b64, bsp_idx_b64, bsp_spawn) = match bsp_bytes {
        Some(bytes) => match extract_bsp_from_bytes(bytes) {
            Some((v, i, nv, nt, spawn)) => {
                eprintln!(
                    "  BSP: {} verts, {} tris, spawn=[{:.1},{:.1},{:.1}]",
                    nv, nt, spawn[0], spawn[1], spawn[2]
                );
                (v, i, spawn)
            }
            None => {
                eprintln!("  BSP extraction failed");
                (String::new(), String::new(), [0.0f32; 3])
            }
        },
        None => (String::new(), String::new(), [0.0f32; 3]),
    };

    // Stride-sample usercmds
    let n = all_cmds.len();
    let stride = ((n + MAX_CMDS_EMBED - 1) / MAX_CMDS_EMBED).max(1);
    let mut idx_sample: Vec<usize> = (0..n).step_by(stride).collect();
    if idx_sample.last().copied().unwrap_or(0) != n.saturating_sub(1) && n > 0 {
        idx_sample.push(n - 1);
    }

    let sampled_cmds: Vec<SampledCmd> = idx_sample.iter().map(|&i| SampledCmd {
        tick: all_cmds[i].tick,
        pitch: all_cmds[i].pitch,
        yaw: all_cmds[i].yaw,
        fwd: all_cmds[i].fwd,
        side: all_cmds[i].side,
        btns: all_cmds[i].btns,
        weapon: all_cmds[i].weapon,
    }).collect();

    // Compute life breaks on full cmds, then map to sampled indices
    let (life_breaks_full, tele_breaks_full) = compute_life_breaks(&all_cmds, &game_events);

    // Map full indices → sampled indices
    let map_to_sampled = |full_indices: &[usize]| -> Vec<usize> {
        let mut out: HashSet<usize> = HashSet::new();
        for &full_idx in full_indices {
            // Binary search in idx_sample for closest >= full_idx
            let mut lo = 0usize;
            let mut hi = idx_sample.len();
            while lo < hi {
                let mid = (lo + hi) >> 1;
                if idx_sample[mid] < full_idx {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            out.insert(lo.min(idx_sample.len().saturating_sub(1)));
        }
        let mut v: Vec<usize> = out.into_iter().collect();
        v.sort_unstable();
        v
    };

    let life_breaks_sampled = map_to_sampled(&life_breaks_full);
    let tele_breaks_sampled = map_to_sampled(&tele_breaks_full);

    // Extract spectator-switch intervals from svc_SetView net messages
    let setview_events = extract_svc_setview(&data, &game_packet_ticks);
    let switch_intervals = spectator_switch_intervals(&setview_events);
    if !switch_intervals.is_empty() {
        eprintln!("  Spectator switches detected: {} interval(s)", switch_intervals.len());
    }
    let view_switches_json = {
        let parts: Vec<String> = switch_intervals.iter()
            .map(|(s, e)| format!("[{},{}]", s, e))
            .collect();
        format!("[{}]", parts.join(","))
    };

    // Build JSON
    let demo_name = name_hint.to_string();
    let meta_json = meta_to_json(
        &header.map_name,
        &header.client_name,
        &header.server_name,
        &header.game_dir,
        header.demo_protocol,
        header.playback_time,
        n,
        tick_rate,
        jump_threshold,
    );
    let cmds_json = cmds_to_json(&sampled_cmds);
    let life_breaks_json = breaks_to_json(&life_breaks_sampled);
    let tele_breaks_json = breaks_to_json(&tele_breaks_sampled);
    let events_json = events_to_json(&game_events);
    let spawn_json = spawn_to_json(bsp_spawn);

    // Build HTML
    let mut html = HTML_TEMPLATE.to_string();
    html = html.replace("__DEMO_NAME__", &escape_html(&demo_name));
    html = html.replace("__META__", &meta_json);
    html = html.replace("__CMDS__", &cmds_json);
    html = html.replace("__LIFE_BREAKS__", &life_breaks_json);
    html = html.replace("__TELEPORT_BREAKS__", &tele_breaks_json);
    html = html.replace("__EVENTS__", &events_json);
    html = html.replace("__WORLD_POSITIONS__", &world_positions_to_json(&world_positions));
    html = html.replace("__BSP_VERTS__", &format!("\"{}\"", bsp_verts_b64));
    html = html.replace("__BSP_IDX__", &format!("\"{}\"", bsp_idx_b64));
    html = html.replace("__BSP_SPAWN__", &spawn_json);
    // Multi-player entity tracks - always extracted now. If the native decoder
    // can't make sense of the demo we fall back to empty objects so the
    // template still parses (legacy single-POV view will still render).
    let (multi_tracks_json, multi_names_json, multi_life_json, multi_obs_json, multi_yaws_json, multi_weps_json, multi_wep_classes_json, primary_eid_json, view_angles_json) = {
        eprint!("  Extracting multi-player tracks ...");
        io::stderr().flush().ok();
        match multi_player::extract_from_bytes(data) {
            Ok(data) => {
                let count = data.tracks.len();
                let total: usize = data.tracks.values().map(|v| v.len()).sum();
                let life_total: usize = data.life_states.values().map(|v| v.len()).sum();
                let yaw_total: usize = data.yaws.values().map(|v| v.len()).sum();
                let wep_total: usize = data.weapons.values().map(|v| v.len()).sum();
                let primary_str = data.primary_entity
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "null".to_string());
                eprintln!(" {} entities, {} samples, {} named, {} life transitions, {} yaw samples, {} weapon switches, {} weapon classes, primary={}",
                    count, total, data.names.len(), life_total, yaw_total, wep_total, data.weapon_classes.len(), primary_str);
                (
                    multi_tracks_to_json(&data),
                    multi_names_to_json(&data),
                    multi_life_states_to_json(&data),
                    multi_observer_modes_to_json(&data),
                    multi_yaws_to_json(&data),
                    multi_weapons_to_json(&data),
                    multi_weapon_classes_to_json(&data),
                    primary_str,
                    view_angles_to_json(&data),
                )
            }
            Err(e) => {
                eprintln!(" failed: {}", e);
                ("{}".to_string(), "{}".to_string(), "{}".to_string(), "{}".to_string(), "{}".to_string(), "{}".to_string(), "{}".to_string(), "null".to_string(), "[]".to_string())
            }
        }
    };
    html = html.replace("__ENTITY_TRACKS__", &multi_tracks_json);
    html = html.replace("__ENTITY_NAMES__", &multi_names_json);
    html = html.replace("__ENTITY_LIFE_STATES__", &multi_life_json);
    html = html.replace("__ENTITY_OBSERVER__", &multi_obs_json);
    html = html.replace("__ENTITY_YAWS__", &multi_yaws_json);
    html = html.replace("__ENTITY_WEAPONS__", &multi_weps_json);
    html = html.replace("__WEAPON_CLASSES__", &multi_wep_classes_json);
    html = html.replace("__PRIMARY_ENTITY__", &primary_eid_json);
    html = html.replace("__VIEW_ANGLES__", &view_angles_json);
    html = html.replace("__VIEW_SWITCHES__", &view_switches_json);

    Ok(html)
}

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
        Some(bytes) => match super::bsp::extract_any_bsp(bytes) {
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
    html = html.replace(
        "__PRIMARY_ENTITY__",
        &mpd.primary_entity.map(|e| e.to_string()).unwrap_or_else(|| "null".to_string()),
    );
    html = html.replace("__VIEW_ANGLES__", &view_angles_to_json(mpd));
    html = html.replace("__VIEW_SWITCHES__", "[]");
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
    let entity_tracks_json = {
        let pts: Vec<String> = world_positions
            .iter()
            .map(|(t, x, y, z)| format!("[{},{},{},{}]", t, json_f32(*x), json_f32(*y), json_f32(*z)))
            .collect();
        format!("{{\"{}\":[{}]}}", REC_EID, pts.join(","))
    };
    let rec_name = if meta.game_dir.is_empty() { "recorder".to_string() } else { format!("{} recorder", meta.game_dir) };
    let entity_names_json = format!(
        "{{\"{}\":{{\"name\":\"{}\",\"steam_id\":\"\",\"user_id\":{},\"is_fake\":false,\"is_hltv\":false,\"aliases\":[\"{}\"]}}}}",
        REC_EID, escape_json_str(&rec_name), REC_EID, escape_json_str(&rec_name),
    );
    let entity_yaws_json = {
        // ENTITY_YAWS shape is [tick, yaw, pitch].
        let pts: Vec<String> = yaws.iter().map(|(t, y, p)| format!("[{},{:.1},{:.1}]", t, y, p)).collect();
        format!("{{\"{}\":[{}]}}", REC_EID, pts.join(","))
    };
    let view_angles_json = {
        // VIEW_ANGLES shape is [tick, pitch, yaw].
        let parts: Vec<String> = view_angles.iter().map(|(t, p, y)| format!("[{},{:.2},{:.2}]", t, p, y)).collect();
        format!("[{}]", parts.join(","))
    };
    // No recorder track decoded (e.g. a header-only / odd demo) → fall back to
    // empty entities so the template still parses and shows the metadata.
    let has_track = !world_positions.is_empty();

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
        Some(bytes) => match super::bsp::extract_goldsrc_bsp_from_bytes(bytes) {
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
    html = html.replace("__PRIMARY_ENTITY__", if has_track { "1" } else { "null" });
    html = html.replace("__VIEW_ANGLES__", &view_angles_json);
    html = html.replace("__VIEW_SWITCHES__", "[]");
    Ok(html)
}
