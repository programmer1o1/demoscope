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

use super::super::util::bytes::{le_f32, le_i32};
use super::super::util::constants::{HEADER_SIZE, SPLIT_SIZE};
use super::super::source::events::{
    compute_life_breaks, display_events_for_game, extract_events_from_payload,
    scan_for_game_event_list, EventField, EventValue, GameEvent, SampledCmd,
};
use super::super::bsp::{extract_bsp_from_bytes, find_bsp_file};
use super::super::header::{parse_header, parse_usercmd};
use super::json::{
    breaks_to_json, cmds_to_json, escape_html, escape_json_str, events_to_json, json_f32,
    meta_to_json, multi_life_states_to_json, multi_names_to_json, multi_observer_modes_to_json,
    multi_tracks_to_json, multi_weapon_classes_to_json, multi_weapons_to_json, multi_yaws_to_json,
    spawn_to_json, view_angles_to_json, world_positions_to_json,
};
use super::super::source::packets::{
    detect_splitscreen, extract_svc_setview, iterate_demo_packets, parse_userinfo_from_demo,
    spectator_switch_intervals,
};
use super::super::source::{self, multi_player};
use super::super::source::multi_player::PlayerMeta;
use super::super::{goldsrc, quake, source2};

// Diff-overlay ghost decoding and the Quake/GoldSrc generators live in
// submodules; the diff helpers are used internally by the Source generator,
// and the non-Source generators are re-exported so `main.rs`/`lib.rs` keep
// reaching them as `render::html::generate_{quake,goldsrc}_html`.
mod diff;
mod engines;
use diff::{extract_ghosts, merge_json_obj, Ghost};
pub use engines::{generate_goldsrc_html, generate_quake_html, generate_source2_html};

const HTML_TEMPLATE: &str = include_str!("template.html");

const MAX_CMDS_EMBED: usize = 20_000;

// CLI wrapper - reads the dem + (optional) bsp from disk, delegates to the
// pure-bytes core, then writes the result. Keeps the existing command-line
// behaviour intact while the WASM build calls the core directly.
pub(crate) fn generate_html(dem_path: &Path, output_path: &Path, jump_threshold: f32, diff_path: Option<&Path>, diff_split: bool) -> io::Result<()> {
    eprintln!("Reading {} ...", dem_path.file_name().unwrap_or_default().to_string_lossy());
    let mut file = File::open(dem_path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    // Optional second demo to overlay (--diff). Read up front; only the Source
    // path consumes it. Its display name is the file stem.
    let diff_bytes: Option<Vec<u8>> = match diff_path {
        Some(p) => {
            eprintln!("  Diff overlay: {}", p.file_name().unwrap_or_default().to_string_lossy());
            Some(std::fs::read(p)?)
        }
        None => None,
    };
    let diff_name: String = diff_path
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let name_hint = dem_path.file_name().unwrap_or_default().to_string_lossy().into_owned();

    // Source 2 (`PBDEMS2`: CS2 / Dota 2 / Deadlock) — a different container from
    // HL2DEMO. Checked FIRST: its 8-byte magic is unambiguous, whereas the Quake
    // route below matches by `.dem` extension and would otherwise swallow it.
    // Metadata-only viewer for now (map / duration / build); position tracks
    // await the entity pipeline.
    if source2::is_source2(&data) {
        // The CS2 world geometry overlay is resolved inside the generator (it
        // needs the map name, which only appears after the entity parse) by
        // looking for `<map>.vpk` beside the demo.
        let html = generate_source2_html(&data, None, Some(dem_path), &name_hint)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let mut out_file = File::create(output_path)?;
        out_file.write_all(html.as_bytes())?;
        eprintln!("HTML -> {}  ({:.1} KB)", output_path.display(), html.len() as f64 / 1024.0);
        return Ok(());
    }

    // Quake-family demos (Q1/Q2/Q3) are a different format from HL2DEMO; route
    // them to the dedicated decoder, which emits the same HTML viewer.
    if diff_path.is_some() && (quake::detect(&name_hint, &data).is_some() || goldsrc::is_goldsrc(&data)) {
        eprintln!("  Note: --diff is currently supported only for Source (HL2DEMO) demos; ignoring");
    }
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
    let html = match diff_bytes.as_deref() {
        // --diff-split: side-by-side dual viewer (two full viewers, frame-locked).
        // Both demos are the same map, so demo 1's BSP serves both panes.
        Some(d2) if diff_split => generate_dual_html_string(
            &data, bsp_bytes.as_deref(), &name_hint,
            d2, bsp_bytes.as_deref(), &diff_name, jump_threshold,
        )?,
        // --diff: overlay demo 2's entities as translucent ghosts in one scene.
        Some(d2) => generate_html_string(
            &data, bsp_bytes.as_deref(), &name_hint, jump_threshold, Some((d2, &diff_name)),
        )?,
        None => generate_html_string(&data, bsp_bytes.as_deref(), &name_hint, jump_threshold, None)?,
    };
    let mut out_file = File::create(output_path)?;
    out_file.write_all(html.as_bytes())?;
    let size_kb = html.len() as f64 / 1024.0;
    eprintln!("HTML -> {}  ({:.1} KB)", output_path.display(), size_kb);
    Ok(())
}

const DUAL_TEMPLATE: &str = include_str!("dual_template.html");

/// Build the side-by-side **dual viewer** (`--diff`): two complete, independent
/// viewers (each its own scene, camera, kills, events, timeline) embedded as
/// base64 blobs in a shell that frame-locks them to one master race-clock. Each
/// inner viewer runs in `#sync` follower mode. Source (HL2DEMO) demos only.
pub fn generate_dual_html_string(
    demo_a: &[u8],
    bsp_a: Option<&[u8]>,
    name_a: &str,
    demo_b: &[u8],
    bsp_b: Option<&[u8]>,
    name_b: &str,
    jump_threshold: f32,
) -> io::Result<String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let viewer_a = generate_html_string(demo_a, bsp_a, name_a, jump_threshold, None)?;
    let viewer_b = generate_html_string(demo_b, bsp_b, name_b, jump_threshold, None)?;
    // JS string-escape for the pane labels (the base64 blobs need no escaping).
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace(['\n', '\r'], " ");
    let html = DUAL_TEMPLATE
        .replace("__VIEWER_A__", &STANDARD.encode(viewer_a.as_bytes()))
        .replace("__VIEWER_B__", &STANDARD.encode(viewer_b.as_bytes()))
        .replace("__NAME_A__", &esc(name_a))
        .replace("__NAME_B__", &esc(name_b));
    Ok(html)
}

// Pure-bytes core: takes the demo + optional BSP as byte slices, returns the
// generated HTML as a String. No filesystem access - used by both the CLI
// wrapper above and the WASM entry point in lib.rs.
//
// `diff` overlays a second demo's entities as translucent "ghosts" in the same
// scene (same map, tick-aligned to this demo's start) for side-by-side
// comparison without splitting the display.
pub fn generate_html_string(
    data: &[u8],
    bsp_bytes: Option<&[u8]>,
    name_hint: &str,
    jump_threshold: f32,
    diff: Option<(&[u8], &str)>,
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
                // proto-4 (L4D-era enum) inserted DEM_CustomData at 8 and shifted
                // DEM_StringTables to 9; proto-3 keeps StringTables at 8 and has no
                // CustomData. CustomData is callbackIndex(4) + length(4) + data —
                // reading its length as if it were a plain length-prefixed block
                // (at p, not p+4) yields garbage and desyncs the walk. Portal 2
                // demos DO carry a CustomData frame after signon, so the old
                // uniform handling bailed right there, before any usercmd — which
                // is why Portal 2 input came back empty. Skip CustomData correctly.
                8 if proto > 3 => {
                    let p = offset + pkt_hdr;
                    if p + 8 > data.len() { break; }
                    let length = le_i32(&data, p + 4);
                    if length < 0 { break; }
                    offset = p.saturating_add(8).saturating_add(length as usize).min(data.len());
                }
                // 6=DataTables (both), 8=StringTables (proto-3), 9=StringTables
                // (proto-4). All plain length-prefixed blocks.
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
    let p2_engine = source::datatable::is_portal2_engine(&header.game_dir);
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

    // CS:GO protobuf-wraps its game events (CSVCMsg_GameEvent), which the
    // bit-packed scanner above can't read. Decode them from the protobuf message
    // stream (same proto as CS2/Source 2; ids 25/30).
    if header.game_dir.eq_ignore_ascii_case("csgo") {
        let csgo_evs = source::csgo::events::decode_events(&signon_payloads, &game_packet_ticks, &data);
        game_events.extend(csgo_evs);
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
    let (mut multi_tracks_json, mut multi_names_json, mut multi_life_json, multi_obs_json, mut multi_yaws_json, multi_weps_json, multi_wep_classes_json, primary_eid_json, view_angles_json) = {
        eprint!("  Extracting multi-player tracks ...");
        io::stderr().flush().ok();
        match multi_player::extract_from_bytes(data) {
            Ok(mut data) => {
                // Fallback name source. GOTV CS:GO demos ship no DEM_STRINGTABLES
                // snapshot, but the CS:GO protobuf string-table decode
                // (csgo::stringtables) now resolves their names directly from
                // svc_*StringTable — including players connected before the
                // recording, which `player_connect` events can't cover. This
                // backfill only fills any tracked slot still unnamed after that
                // (it never overrides), so it's a no-op on every current test demo
                // and stays purely as a safety net.
                backfill_names_from_connects(&mut data.names, &data.tracks, &game_events);
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
    // ── Side-by-side diff: overlay the second demo's entities as translucent
    // ghosts in this scene. Ghost eids are offset by 900000; their ticks are
    // shifted so both runs start together.
    let mut ghost_eids: Vec<u32> = Vec::new();
    if let Some((d2, d2name)) = diff {
        let ghosts = extract_ghosts(d2, d2name);
        if ghosts.is_empty() {
            eprintln!("  --diff: no tracks decoded from '{}' (skipped)", d2name);
        } else {
            let d1_anchor = all_cmds
                .first()
                .map(|c| c.tick)
                .or_else(|| world_positions.first().map(|p| p.0))
                .unwrap_or(0);
            let d2_anchor = ghosts
                .iter()
                .filter_map(|g| g.samples.first().map(|s| s.0))
                .min()
                .unwrap_or(0);
            let shift = d1_anchor - d2_anchor;
            for g in &ghosts {
                ghost_eids.push(g.eid);
                let pos_s = multi_player::subsample(&g.samples, 6000);
                let pts: Vec<String> = pos_s
                    .iter()
                    .map(|(t, x, y, z)| format!("[{},{},{},{}]", t + shift, json_f32(*x), json_f32(*y), json_f32(*z)))
                    .collect();
                multi_tracks_json = merge_json_obj(&multi_tracks_json, g.eid, &format!("[{}]", pts.join(",")));
                let ystride = (g.yaws.len() / 6000).max(1);
                let ypts: Vec<String> = g.yaws
                    .iter()
                    .step_by(ystride)
                    .map(|(t, y, p)| format!("[{},{:.1},{:.1}]", t + shift, y, p))
                    .collect();
                multi_yaws_json = merge_json_obj(&multi_yaws_json, g.eid, &format!("[{}]", ypts.join(",")));
                let nm = format!(
                    "{{\"name\":\"{}\",\"steam_id\":\"\",\"user_id\":{},\"is_fake\":false,\"is_hltv\":false,\"aliases\":[\"{}\"]}}",
                    escape_json_str(&g.name), g.eid, escape_json_str(&g.name),
                );
                multi_names_json = merge_json_obj(&multi_names_json, g.eid, &nm);
                if !g.life.is_empty() {
                    let lpts: Vec<String> = g.life.iter().map(|(t, s)| format!("[{},{}]", t + shift, s)).collect();
                    multi_life_json = merge_json_obj(&multi_life_json, g.eid, &format!("[{}]", lpts.join(",")));
                }
            }
            eprintln!(
                "  --diff: overlaid {} ghost{} from '{}' (tick shift {})",
                ghosts.len(), if ghosts.len() == 1 { "" } else { "s" }, d2name, shift
            );
        }
    }
    let ghost_eids_json = format!(
        "[{}]",
        ghost_eids.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(",")
    );
    html = html.replace("__GHOST_EIDS__", &ghost_eids_json);
    html = html.replace("__ENTITY_TRACKS__", &multi_tracks_json);
    html = html.replace("__ENTITY_NAMES__", &multi_names_json);
    html = html.replace("__ENTITY_LIFE_STATES__", &multi_life_json);
    html = html.replace("__ENTITY_OBSERVER__", &multi_obs_json);
    html = html.replace("__ENTITY_YAWS__", &multi_yaws_json);
    html = html.replace("__ENTITY_WEAPONS__", &multi_weps_json);
    html = html.replace("__WEAPON_CLASSES__", &multi_wep_classes_json);
    // Source 1 carries real per-tick buttons via CMDS for the recorder; no
    // per-entity networked button stream is decoded here, so this is empty and
    // the viewer falls back to CMDS / velocity-derivation as before.
    html = html.replace("__ENTITY_BUTTONS__", "{}");
    html = html.replace("__PRIMARY_ENTITY__", &primary_eid_json);
    html = html.replace("__VIEW_ANGLES__", &view_angles_json);
    html = html.replace("__VIEW_SWITCHES__", &view_switches_json);

    Ok(html)
}


/// Fallback name source: backfill from `player_connect` game events for any
/// tracked slot still unnamed after the primary decoders. `player_connect`
/// carries `name` + slot `index` + `userid`, and the tracked entity id is
/// `index + 1`. Fills only not-yet-named slots, so it never overrides names from
/// DEM_STRINGTABLES (POV) or the CS:GO protobuf string-tables (GOTV). Since the
/// latter now resolves GOTV names directly, this is a no-op on current demos and
/// remains only to cover a slot those paths might miss.
fn backfill_names_from_connects(
    names: &mut std::collections::HashMap<u32, PlayerMeta>,
    tracks: &std::collections::HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    events: &[GameEvent],
) {
    for ev in events {
        if ev.event != "player_connect" {
            continue;
        }
        let (mut nm, mut index, mut uid): (Option<String>, Option<i32>, Option<i32>) = (None, None, None);
        for f in &ev.fields {
            match f.name.as_str() {
                "name" => if let EventValue::Str(s) = &f.value { nm = Some(s.clone()); },
                "index" => if let EventValue::Int(n) = &f.value { index = Some(*n); },
                "userid" => if let EventValue::Int(n) = &f.value { uid = Some(*n); },
                _ => {}
            }
        }
        let (nm, idx) = match (nm, index) {
            (Some(n), Some(i)) if !n.is_empty() && (0..64).contains(&i) => (n, i),
            _ => continue,
        };
        let eid = (idx + 1) as u32;
        if !tracks.contains_key(&eid) {
            continue; // only label entities we actually track
        }
        if names.get(&eid).is_some_and(|p| !p.name.is_empty()) {
            continue; // already named (e.g. POV demo via DEM_STRINGTABLES)
        }
        names.insert(eid, PlayerMeta {
            name: nm.clone(),
            user_id: uid.unwrap_or(0).max(0) as u32,
            steam_id: String::new(),
            is_fake: false,
            is_hltv: false,
            aliases: vec![nm],
        });
    }
}
