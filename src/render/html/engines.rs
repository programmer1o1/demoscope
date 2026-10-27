// Non-Source HTML generators: the Quake-family (Q1/Q2/Q3) and GoldSrc (HL1)
// paths. Both fill the SAME `template.html` as the Source generator in the
// parent `html` module, reusing its JSON serializers and the shared BSP
// dispatcher, so the 3D viewer consumes their output identically.

use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};

use super::super::super::bsp::{
    GoldSrcMapInfo, extract_any_bsp, extract_goldsrc_bsp_from_bytes, find_vpk_file,
    goldsrc_alignment_offsets, goldsrc_map_info, map_layers,
};
use super::super::super::{goldsrc, quake, source2};
use super::super::json::{
    escape_html, escape_json_str, events_to_json, json_f32, meta_to_json,
    multi_life_states_to_json, multi_names_to_json, multi_observer_modes_to_json,
    multi_tracks_to_json, multi_tracks_to_json_with, multi_weapon_classes_to_json,
    multi_weapons_to_json, multi_yaws_to_json, multi_yaws_to_json_with, spawn_to_json,
    view_angles_to_json, view_angles_to_json_with, world_positions_to_json,
};
use super::HTML_TEMPLATE;

// Quake demos are long (15-20 minute matches) and record players at server
// frame rate, so the default per-player budget (4000 points, one every ~0.3s on
// a 20 minute demo) turns smooth movement into straight segments and makes the
// first-person aim step. Budget by duration instead: this many points per
// second, within a floor and a ceiling that bound the page size.
const QUAKE_POINTS_PER_SEC: f32 = 12.0;
const QUAKE_MIN_POINTS: usize = 4000;
const QUAKE_MAX_POINTS: usize = 20_000;

fn quake_point_budget(duration_secs: f32) -> usize {
    ((duration_secs.max(0.0) * QUAKE_POINTS_PER_SEC) as usize)
        .clamp(QUAKE_MIN_POINTS, QUAKE_MAX_POINTS)
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

    // WORLD_POSITIONS stays empty: with no usercmds the template builds its
    // playback timeline and recorder path from the primary's entity track, so
    // sending a second copy only duplicates data.
    let budget = quake_point_budget(meta.duration);

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
    html = html.replace("__WORLD_POSITIONS__", "[]");
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
    html = html.replace("__ENTITY_TRACKS__", &multi_tracks_to_json_with(mpd, budget));
    html = html.replace("__ENTITY_NAMES__", &multi_names_to_json(mpd));
    html = html.replace("__ENTITY_LIFE_STATES__", &multi_life_states_to_json(mpd));
    html = html.replace("__ENTITY_OBSERVER__", &multi_observer_modes_to_json(mpd));
    html = html.replace("__ENTITY_YAWS__", &multi_yaws_to_json_with(mpd, budget));
    html = html.replace("__ENTITY_WEAPONS__", &multi_weapons_to_json(mpd));
    html = html.replace("__WEAPON_CLASSES__", &multi_weapon_classes_to_json(mpd));
    html = html.replace("__ENTITY_BUTTONS__", "{}"); // Quake demos carry no button state
    html = html.replace(
        "__PRIMARY_ENTITY__",
        &mpd.primary_entity
            .map(|e| e.to_string())
            .unwrap_or_else(|| "null".to_string()),
    );
    html = html.replace("__VIEW_ANGLES__", &view_angles_to_json_with(mpd, budget));
    html = html.replace("__VIEW_SWITCHES__", "[]");
    html = html.replace("__GHOST_EIDS__", "[]");
    Ok(html)
}

/// The Source 2 header stores the recorder's absolute game directory (for
/// example `D:\\SteamLibrary\\...\\citadel`). Only the last component is kept so
/// a shared HTML file doesn't carry a local path. Handles both separators.
fn final_path_component(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
}

// Source 2 (`PBDEMS2`: CS2 / Dota 2 / Deadlock) HTML generator.
//
// Metadata comes from the container header, `CDemoFileHeader` (map / server /
// build) and `CDemoFileInfo` (duration / ticks). Player tracks, names, events
// and (on CS2) inputs and held weapons come from the entity pipeline
// (FlattenedSerializer + field-path Huffman + PacketEntities). The SAME
// template as the other engines is filled; sections a game doesn't provide
// are left empty.
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
        meta.map_name,
        meta.server_name,
        meta.build_num,
        meta.demo_version_name,
        meta.playback_time,
        meta.playback_ticks,
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
    let deaths: usize = tracks
        .life_states
        .values()
        .flatten()
        .filter(|(_, s)| *s != 0)
        .count();
    eprintln!(
        "  Source 2 entities: {} player tracks ({} named), {} samples ({} packets ok / {} failed)",
        tracks.tracks.len(),
        named,
        total,
        tracks.pe_ok,
        tracks.pe_fail
    );
    let kills = tracks
        .events
        .iter()
        .filter(|e| e.event == "player_death")
        .count();
    let bombs = tracks
        .events
        .iter()
        .filter(|e| e.event.starts_with("bomb_"))
        .count();
    eprintln!(
        "  Source 2 gameplay: {} events ({} kills, {} bomb), {} death transitions, {} rounds",
        tracks.events.len(),
        kills,
        bombs,
        deaths,
        tracks.rounds.last().map(|(_, r)| *r).unwrap_or(0),
    );
    for econ in tracks.econ.values() {
        eprintln!(
            "    {}: {}K/{}D/{}A, ${}, team {}",
            econ.name, econ.kills, econ.deaths, econ.assists, econ.money, econ.team
        );
    }
    // Prefer the map name recovered from svc_ServerInfo (the header is often blank
    // on loopback recordings).
    let map_name = if !tracks.map_name.is_empty() {
        tracks.map_name.clone()
    } else {
        meta.map_name.clone()
    };

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
            if *tk <= last {
                continue;
            }
            last = *tk;
            pts.push(format!(
                "[{},{},{},{}]",
                tk,
                json_f32(*x),
                json_f32(*y),
                json_f32(*z)
            ));
        }
        track_parts.push(format!("\"{}\":[{}]", eid, pts.join(",")));

        if let Some(ys) = tracks.yaws.get(eid) {
            let mut last = i32::MIN;
            let mut yp = Vec::new();
            for (tk, yaw, pitch) in ys {
                if *tk <= last {
                    continue;
                }
                last = *tk;
                yp.push(format!("[{},{:.1},{:.1}]", tk, yaw, pitch));
            }
            yaw_parts.push(format!("\"{}\":[{}]", eid, yp.join(",")));
        }

        let nm = tracks
            .names
            .get(eid)
            .cloned()
            .unwrap_or_else(|| format!("player {}", eid));
        // Attach the player's latest scoreboard/economy so the sidebar can show it.
        let (k, d, a, money, score, team) = tracks
            .econ_by_pawn
            .get(eid)
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
    let primary_json = eids
        .iter()
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
    // completed round from it and merge into the event list: this is what makes
    // "round end" show up for CS2 (CS:GO/CS:S carry the real events via Source 1).
    let round_markers: Vec<String> = tracks
        .rounds
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(tick, n)| {
            format!(
                "{{\"event\":\"round_end\",\"tick\":{},\"round\":{}}}",
                tick, n
            )
        })
        .collect();
    let events_json = {
        let real = events_to_json(&tracks.events);
        let inner = real
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim();
        let mut parts: Vec<String> = Vec::new();
        if !inner.is_empty() {
            parts.push(inner.to_string());
        }
        if !round_markers.is_empty() {
            parts.push(round_markers.join(","));
        }
        format!("[{}]", parts.join(","))
    };

    let meta_json = meta_to_json(
        &map_name,
        &meta.client_name,
        &meta.server_name,
        final_path_component(&meta.game_directory),
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
    // loopback recordings), so the on-disk pak is resolved here: the explicit
    // `vpk_bytes` (the browser drag-drop buffer) takes priority.
    let resolved_vpk: Option<Vec<u8>> = if vpk_bytes.is_some() {
        None
    } else {
        dem_path
            .filter(|_| !map_name.is_empty())
            .and_then(|p| find_vpk_file(p, &map_name))
            .and_then(|p| {
                eprintln!(
                    "  Found map pak: {}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                );
                std::fs::read(&p).ok()
            })
    };
    let vpk = vpk_bytes.or(resolved_vpk.as_deref());
    let (s2_verts, s2_idx) = match vpk.and_then(source2::map::extract_map_geometry) {
        Some((v, i, nv, nt)) => {
            eprintln!(
                "  Source 2 map: {} verts, {} triangles from .vpk world_physics",
                nv, nt
            );
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
    html = html.replace(
        "__ENTITY_TRACKS__",
        if has_track { &entity_tracks_json } else { "{}" },
    );
    html = html.replace(
        "__ENTITY_NAMES__",
        if has_track { &entity_names_json } else { "{}" },
    );
    html = html.replace(
        "__ENTITY_LIFE_STATES__",
        if has_track {
            &entity_life_states_json
        } else {
            "{}"
        },
    );
    html = html.replace("__ENTITY_OBSERVER__", "{}");
    html = html.replace(
        "__ENTITY_YAWS__",
        if has_track { &entity_yaws_json } else { "{}" },
    );
    // Active-weapon streams (CS2; empty for Deadlock/Dota, which have no held
    // weapon). Shape mirrors the Source 1 path: ENTITY_WEAPONS { pawn: [[tick,
    // weaponClassId], …] } resolved through WEAPON_CLASSES { weaponClassId:
    // "ak47" } by the viewer's weaponAt().
    let weapons_json = {
        let parts: Vec<String> = tracks
            .weapons
            .iter()
            .filter(|(_, sw)| !sw.is_empty())
            .map(|(eid, sw)| {
                let pts: Vec<String> = sw.iter().map(|(t, c)| format!("[{},{}]", t, c)).collect();
                format!("\"{}\":[{}]", eid, pts.join(","))
            })
            .collect();
        format!("{{{}}}", parts.join(","))
    };
    let weapon_classes_json = {
        // `weapon_names` is a HashMap: sort by class id so the output is
        // byte-identical from run to run.
        let mut names: Vec<(&i32, &String)> = tracks
            .weapon_names
            .iter()
            .filter(|(_, n)| !n.is_empty())
            .collect();
        names.sort_by_key(|(cid, _)| **cid);
        let parts: Vec<String> = names
            .into_iter()
            .map(|(cid, n)| format!("\"{}\":\"{}\"", cid, escape_json_str(n)))
            .collect();
        format!("{{{}}}", parts.join(","))
    };
    html = html.replace(
        "__ENTITY_WEAPONS__",
        if has_track { &weapons_json } else { "{}" },
    );
    html = html.replace(
        "__WEAPON_CLASSES__",
        if has_track {
            &weapon_classes_json
        } else {
            "{}"
        },
    );
    // Per-pawn input streams: { pawn: [[tick, mask], …] } decoded from the
    // movement-services button state. Populated on CS2; empty on the sampled
    // Deadlock and Dota 2 demos. The viewer lights actual W/A/S/D/attack/jump/duck
    // from these where present, instead of inferring from velocity.
    let buttons_json = {
        let parts: Vec<String> = tracks
            .buttons
            .iter()
            .filter(|(_, b)| !b.is_empty())
            .map(|(eid, b)| {
                let pts: Vec<String> = b.iter().map(|(t, m)| format!("[{},{}]", t, m)).collect();
                format!("\"{}\":[{}]", eid, pts.join(","))
            })
            .collect();
        format!("{{{}}}", parts.join(","))
    };
    html = html.replace(
        "__ENTITY_BUTTONS__",
        if has_track { &buttons_json } else { "{}" },
    );
    html = html.replace(
        "__PRIMARY_ENTITY__",
        if has_track { &primary_json } else { "null" },
    );
    html = html.replace("__VIEW_ANGLES__", "[]");
    html = html.replace("__VIEW_SWITCHES__", "[]");
    html = html.replace("__GHOST_EIDS__", "[]");
    Ok(html)
}

fn map_contains(info: &GoldSrcMapInfo, point: [f32; 3]) -> bool {
    const MARGIN: f32 = 192.0;
    (0..3).all(|axis| {
        point[axis] >= info.mins[axis] - MARGIN && point[axis] <= info.maxs[axis] + MARGIN
    })
}

fn add_offset(point: [f32; 3], offset: [f32; 3]) -> [f32; 3] {
    [
        point[0] + offset[0],
        point[1] + offset[1],
        point[2] + offset[2],
    ]
}

fn distance_sq(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Stateful raw-map-space → stitched-campaign-space mapper. A changelevel is
/// visible as a large raw-coordinate discontinuity. At that point the BSP
/// whose bounds contain the new sample and whose landmark translation restores
/// continuity wins. Normal teleports stay on the active layer unless another
/// translated layer is substantially closer.
struct GoldSrcPathAligner<'a> {
    infos: &'a [GoldSrcMapInfo],
    offsets: &'a [[f32; 3]],
    active: usize,
    previous: Option<[f32; 3]>,
    switches: usize,
}

impl<'a> GoldSrcPathAligner<'a> {
    fn new(infos: &'a [GoldSrcMapInfo], offsets: &'a [[f32; 3]]) -> Self {
        Self {
            infos,
            offsets,
            active: 0,
            previous: None,
            switches: 0,
        }
    }

    fn apply(&mut self, raw: [f32; 3]) -> [f32; 3] {
        if self.infos.is_empty() || self.offsets.len() != self.infos.len() {
            return raw;
        }
        let mut chosen = self.active.min(self.infos.len() - 1);
        if let Some(previous) = self.previous {
            let active_point = add_offset(raw, self.offsets[chosen]);
            let active_distance = distance_sq(previous, active_point);
            let mut best = chosen;
            let mut best_distance = active_distance;
            for (i, info) in self.infos.iter().enumerate() {
                if !map_contains(info, raw) {
                    continue;
                }
                let candidate_distance = distance_sq(previous, add_offset(raw, self.offsets[i]));
                if candidate_distance < best_distance {
                    best = i;
                    best_distance = candidate_distance;
                }
            }
            let outside_active = !map_contains(&self.infos[chosen], raw);
            let discontinuity = active_distance > 1024.0 * 1024.0;
            if best != chosen
                && (outside_active || (discontinuity && best_distance < active_distance * 0.25))
            {
                chosen = best;
                self.active = best;
                self.switches += 1;
            }
        }
        let aligned = add_offset(raw, self.offsets[chosen]);
        self.previous = Some(aligned);
        aligned
    }
}

fn align_goldsrc_camera(
    samples: &mut [goldsrc::CamSample],
    infos: &[GoldSrcMapInfo],
    offsets: &[[f32; 3]],
) -> usize {
    let mut aligner = GoldSrcPathAligner::new(infos, offsets);
    for (_, x, y, z, _, _) in samples {
        let aligned = aligner.apply([*x, *y, *z]);
        *x = aligned[0];
        *y = aligned[1];
        *z = aligned[2];
    }
    aligner.switches
}

fn align_goldsrc_track(
    samples: &mut [(f32, f32, f32, f32)],
    infos: &[GoldSrcMapInfo],
    offsets: &[[f32; 3]],
) -> usize {
    let mut aligner = GoldSrcPathAligner::new(infos, offsets);
    for (_, x, y, z) in samples {
        let aligned = aligner.apply([*x, *y, *z]);
        *x = aligned[0];
        *y = aligned[1];
        *z = aligned[2];
    }
    aligner.switches
}

#[cfg(test)]
mod goldsrc_alignment_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn switches_track_space_when_landmark_offset_restores_continuity() {
        let infos = vec![
            GoldSrcMapInfo {
                landmarks: HashMap::new(),
                mins: [-100.0; 3],
                maxs: [100.0; 3],
            },
            GoldSrcMapInfo {
                landmarks: HashMap::new(),
                mins: [900.0, -100.0, -100.0],
                maxs: [1100.0, 100.0, 100.0],
            },
        ];
        let offsets = [[0.0; 3], [-1000.0, 0.0, 0.0]];
        let mut track = vec![(0.0, 0.0, 0.0, 0.0), (1.0, 1000.0, 0.0, 0.0)];
        assert_eq!(align_goldsrc_track(&mut track, &infos, &offsets), 1);
        assert_eq!(track[1], (1.0, 0.0, 0.0, 0.0));
    }
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
    let layers = bsp_bytes.map(map_layers).unwrap_or_default();
    generate_goldsrc_html_multi(data, &layers, name_hint)
}

pub fn generate_goldsrc_html_multi(
    data: &[u8],
    bsp_layers: &[&[u8]],
    name_hint: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let meta = goldsrc::parse(data)
        .ok_or_else(|| Box::<dyn std::error::Error>::from("not a valid HLDEMO (GoldSrc) file"))?;
    eprintln!(
        "  GoldSrc demo: map={} game={} demo_proto={} net_proto={} duration={:.1}s frames={}",
        meta.map_name,
        meta.game_dir,
        meta.demo_protocol,
        meta.net_protocol,
        meta.duration,
        meta.frame_count
    );

    let map_infos: Vec<GoldSrcMapInfo> = bsp_layers
        .iter()
        .map(|bytes| goldsrc_map_info(bytes).unwrap_or_default())
        .collect();
    let map_offsets = goldsrc_alignment_offsets(&map_infos);
    for (i, (info, offset)) in map_infos.iter().zip(&map_offsets).enumerate() {
        if i > 0 && offset.iter().any(|v| v.abs() > 0.01) {
            eprintln!(
                "  GoldSrc BSP layer {}: landmark-aligned by ({:.1}, {:.1}, {:.1}) [{} anchors]",
                i + 1,
                offset[0],
                offset[1],
                offset[2],
                info.landmarks.len()
            );
        }
    }

    let tick_rate = if meta.duration > 0.0 && meta.frame_count > 0 {
        meta.frame_count as f32 / meta.duration
    } else {
        100.0
    };

    // Decode the recorder camera path (eye origin + view angles) from the
    // NetMsg RefParams stream, mapped onto a synthetic tick = time × tick_rate.
    let mut cam = goldsrc::extract_camera(data, &meta);
    let camera_switches = align_goldsrc_camera(&mut cam, &map_infos, &map_offsets);
    if camera_switches > 0 {
        eprintln!("  GoldSrc changelevels: {camera_switches} camera segment transition(s) aligned");
    }
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
    eprintln!(
        "  GoldSrc camera: {} samples ({} after per-tick dedup)",
        cam.len(),
        world_positions.len()
    );

    // Feed the recorder as a single synthetic multi-player entity (id 0) - the same shape
    // the Quake path uses. GoldSrc demos carry no usercmds, so `CMDS` is empty
    // and the WORLD_POSITIONS→finalPositions interpolation (which is indexed by
    // CMDS ticks) would yield nothing; rendering the recorder as a real entity
    // track with its own samples sidesteps that and drives the avatar, route,
    // minimap, and follow/FPS camera directly.
    const REC_EID: i32 = 0;
    let mut rec_name = if meta.game_dir.is_empty() {
        "recorder".to_string()
    } else {
        format!("{} recorder", meta.game_dir)
    };
    let view_angles_json = {
        // VIEW_ANGLES shape is [tick, pitch, yaw].
        let parts: Vec<String> = view_angles
            .iter()
            .map(|(t, p, y)| format!("[{},{:.2},{:.2}]", t, p, y))
            .collect();
        format!("[{}]", parts.join(","))
    };

    // Decode the *other* players out of the svc entity stream (delta-compressed
    // PacketEntities). When that yields tracks we render real per-player dots;
    // otherwise we fall back to driving a single avatar from the recorder camera.
    let mut ents = goldsrc::extract_entities(data, &meta);
    let entity_switches: usize = ents
        .tracks
        .values_mut()
        .map(|track| align_goldsrc_track(track, &map_infos, &map_offsets))
        .sum();
    if entity_switches > 0 {
        eprintln!("  GoldSrc changelevels: {entity_switches} player-track transition(s) aligned");
    }
    if let Some(name) = ents.names.get(&1).filter(|name| !name.trim().is_empty()) {
        rec_name = name.clone();
    }
    let camera_has_motion = world_positions
        .first()
        .map(|(_, sx, sy, sz)| {
            world_positions.iter().any(|(_, x, y, z)| {
                (x - sx).abs() > 1.0 || (y - sy).abs() > 1.0 || (z - sz).abs() > 1.0
            })
        })
        .unwrap_or(false);
    // Include a synthetic recorder only when the camera path is spatially
    // useful. HLTV demos often have a zeroed recorder camera; keeping that out
    // of the player set avoids collisions with real CS player life state.
    if !ents.tracks.is_empty() && camera_has_motion {
        ents.tracks
            .entry(REC_EID as u32)
            .or_insert_with(|| cam.iter().map(|&(t, x, y, z, _, _)| (t, x, y, z)).collect());
        ents.yaws
            .entry(REC_EID as u32)
            .or_insert_with(|| cam.iter().map(|&(t, _, _, _, p, y)| (t, y, p)).collect());
        ents.names
            .entry(REC_EID as u32)
            .or_insert_with(|| rec_name.clone());
        ents.primary = Some(REC_EID as u32);
    }
    let to_tick = |time: f32| (time * tick_rate).round() as i32;
    let entity_tracks_json;
    let entity_names_json;
    let entity_life_states_json;
    let entity_yaws_json;
    let entity_weapons_json;
    let weapon_classes_json;
    let primary_json: String;
    let has_track;
    if !ents.tracks.is_empty() {
        let mut eids: Vec<u32> = ents.tracks.keys().copied().collect();
        eids.sort_unstable();
        let mut track_parts = Vec::with_capacity(eids.len());
        let mut yaw_parts = Vec::with_capacity(eids.len());
        let mut name_parts = Vec::with_capacity(eids.len());
        let mut life_parts = Vec::new();
        let mut weapon_parts = Vec::new();
        let mut total_samples = 0usize;
        let mut life_total = 0usize;
        let mut death_total = 0usize;
        for eid in &eids {
            let life_rows: Vec<(i32, u8)> = ents
                .life_states
                .get(eid)
                .map(|states| {
                    let mut rows: Vec<(i32, u8)> = states
                        .iter()
                        .map(|(time, state)| (to_tick(*time), *state))
                        .collect();
                    rows.sort_by_key(|(tk, _)| *tk);
                    let mut collapsed: Vec<(i32, u8)> = Vec::new();
                    for (tk, state) in rows {
                        if let Some(last) = collapsed.last_mut() {
                            if last.0 == tk {
                                last.1 = state;
                                continue;
                            }
                            if last.1 == state {
                                continue;
                            }
                        }
                        collapsed.push((tk, state));
                    }
                    collapsed
                })
                .unwrap_or_default();
            let alive_at = |tk: i32| -> bool {
                if life_rows.is_empty() {
                    return true;
                }
                let mut alive = life_rows[0].1 != 0;
                for (lt, state) in &life_rows {
                    if *lt > tk {
                        break;
                    }
                    alive = *state == 0;
                }
                alive
            };
            // Per-tick dedup keeps the viewer's tick-indexed lookups monotonic.
            let mut last = i32::MIN;
            let mut pts = Vec::new();
            for (time, x, y, z) in &ents.tracks[eid] {
                let tk = to_tick(*time);
                if tk <= last {
                    continue;
                }
                last = tk;
                if !alive_at(tk) {
                    continue;
                }
                pts.push(format!(
                    "[{},{},{},{}]",
                    tk,
                    json_f32(*x),
                    json_f32(*y),
                    json_f32(*z)
                ));
            }
            total_samples += pts.len();
            track_parts.push(format!("\"{}\":[{}]", eid, pts.join(",")));
            if let Some(ys) = ents.yaws.get(eid) {
                let mut last = i32::MIN;
                let mut yp = Vec::new();
                for (time, yaw, pitch) in ys {
                    let tk = to_tick(*time);
                    if tk <= last {
                        continue;
                    }
                    last = tk;
                    if !alive_at(tk) {
                        continue;
                    }
                    yp.push(format!("[{},{:.1},{:.1}]", tk, yaw, pitch));
                }
                yaw_parts.push(format!("\"{}\":[{}]", eid, yp.join(",")));
            }
            let nm = ents
                .names
                .get(eid)
                .cloned()
                .unwrap_or_else(|| format!("player {}", eid));
            name_parts.push(format!(
                "\"{}\":{{\"name\":\"{}\",\"steam_id\":\"\",\"user_id\":{},\"is_fake\":false,\"is_hltv\":false,\"aliases\":[\"{}\"]}}",
                eid, escape_json_str(&nm), eid, escape_json_str(&nm),
            ));
            if !life_rows.is_empty() {
                let mut lp = Vec::new();
                for (tk, state) in &life_rows {
                    lp.push(format!("[{},{}]", tk, state));
                    life_total += 1;
                    if *state != 0 {
                        death_total += 1;
                    }
                }
                if !lp.is_empty() {
                    life_parts.push(format!("\"{}\":[{}]", eid, lp.join(",")));
                }
            }
            if let Some(ws) = ents.weapons.get(eid) {
                let mut last = i32::MIN;
                let mut wp = Vec::new();
                for (time, wid) in ws {
                    let tk = to_tick(*time);
                    if tk <= last {
                        continue;
                    }
                    last = tk;
                    if !alive_at(tk) {
                        continue;
                    }
                    wp.push(format!("[{},{}]", tk, wid));
                }
                if !wp.is_empty() {
                    weapon_parts.push(format!("\"{}\":[{}]", eid, wp.join(",")));
                }
            }
        }
        entity_tracks_json = format!("{{{}}}", track_parts.join(","));
        entity_yaws_json = format!("{{{}}}", yaw_parts.join(","));
        entity_names_json = format!("{{{}}}", name_parts.join(","));
        entity_life_states_json = format!("{{{}}}", life_parts.join(","));
        entity_weapons_json = format!("{{{}}}", weapon_parts.join(","));
        let mut class_parts: Vec<String> = ents
            .weapon_classes
            .iter()
            .map(|(wid, name)| format!("\"{}\":\"{}\"", wid, escape_json_str(name)))
            .collect();
        class_parts.sort();
        weapon_classes_json = format!("{{{}}}", class_parts.join(","));
        primary_json = ents
            .primary
            .or_else(|| eids.first().copied())
            .unwrap()
            .to_string();
        has_track = true;
        let weapon_total: usize = ents.weapons.values().map(|v| v.len()).sum();
        eprintln!(
            "  GoldSrc entities: {} player tracks, {} samples, {} weapon switches, {} life transitions ({} deaths)",
            eids.len(),
            total_samples,
            weapon_total,
            life_total,
            death_total,
        );
    } else {
        // Recorder-only fallback: render the recorder camera path as entity 0.
        let pts: Vec<String> = world_positions
            .iter()
            .map(|(t, x, y, z)| {
                format!("[{},{},{},{}]", t, json_f32(*x), json_f32(*y), json_f32(*z))
            })
            .collect();
        entity_tracks_json = format!("{{\"{}\":[{}]}}", REC_EID, pts.join(","));
        entity_names_json = format!(
            "{{\"{}\":{{\"name\":\"{}\",\"steam_id\":\"\",\"user_id\":{},\"is_fake\":false,\"is_hltv\":false,\"aliases\":[\"{}\"]}}}}",
            REC_EID,
            escape_json_str(&rec_name),
            REC_EID,
            escape_json_str(&rec_name),
        );
        entity_life_states_json = "{}".to_string();
        let yp: Vec<String> = yaws
            .iter()
            .map(|(t, y, p)| format!("[{},{:.1},{:.1}]", t, y, p))
            .collect();
        entity_yaws_json = format!("{{\"{}\":[{}]}}", REC_EID, yp.join(","));
        entity_weapons_json = "{}".to_string();
        weapon_classes_json = "{}".to_string();
        primary_json = REC_EID.to_string();
        has_track = !world_positions.is_empty();
    }

    let meta_json = meta_to_json(
        &meta.map_name,
        &rec_name,
        "", // no server name field
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
    html = html.replace(
        "__WORLD_POSITIONS__",
        &world_positions_to_json(&world_positions),
    );
    // GoldSrc map geometry (BSP v30 / Q1 v29). Multiple layers are appended
    // into one mesh with adjusted indices, preserving each map's native world
    // coordinates. This is useful for HL1 campaigns split across many BSPs.
    let mut merged_verts = Vec::<u8>::new();
    let mut merged_idx = Vec::<u8>::new();
    let mut bsp_spawn = [0.0f32; 3];
    let mut decoded_layers = 0usize;
    let mut total_tris = 0usize;
    for (layer_index, bytes) in bsp_layers.iter().enumerate() {
        let Some((verts_b64, idx_b64, nv, nt, spawn)) = extract_goldsrc_bsp_from_bytes(bytes)
        else {
            eprintln!("  GoldSrc BSP layer extraction failed; skipping");
            continue;
        };
        let Ok(mut verts) = STANDARD.decode(verts_b64) else {
            continue;
        };
        let Ok(indices) = STANDARD.decode(idx_b64) else {
            continue;
        };
        let offset = map_offsets.get(layer_index).copied().unwrap_or([0.0; 3]);
        for vertex in verts.chunks_exact_mut(12) {
            for axis in 0..3 {
                let at = axis * 4;
                let raw = f32::from_le_bytes(vertex[at..at + 4].try_into().unwrap());
                vertex[at..at + 4].copy_from_slice(&(raw + offset[axis]).to_le_bytes());
            }
        }
        let vertex_offset = (merged_verts.len() / 12) as u32;
        merged_verts.extend_from_slice(&verts);
        for raw in indices.chunks_exact(4) {
            let idx = u32::from_le_bytes(raw.try_into().unwrap()).saturating_add(vertex_offset);
            merged_idx.extend_from_slice(&idx.to_le_bytes());
        }
        if decoded_layers == 0 {
            bsp_spawn = add_offset(spawn, offset);
        }
        decoded_layers += 1;
        total_tris += nt;
        eprintln!(
            "  GoldSrc BSP layer {}: {} verts, {} tris",
            decoded_layers, nv, nt
        );
    }
    if decoded_layers > 1 {
        eprintln!(
            "  GoldSrc map stack: {} layers, {} tris (landmark-aligned when anchors match)",
            decoded_layers, total_tris
        );
    }
    let bsp_verts_b64 = STANDARD.encode(&merged_verts);
    let bsp_idx_b64 = STANDARD.encode(&merged_idx);
    html = html.replace("__BSP_VERTS__", &format!("\"{}\"", bsp_verts_b64));
    html = html.replace("__BSP_IDX__", &format!("\"{}\"", bsp_idx_b64));
    html = html.replace("__BSP_SPAWN__", &spawn_to_json(bsp_spawn));
    html = html.replace(
        "__ENTITY_TRACKS__",
        if has_track { &entity_tracks_json } else { "{}" },
    );
    html = html.replace(
        "__ENTITY_NAMES__",
        if has_track { &entity_names_json } else { "{}" },
    );
    html = html.replace(
        "__ENTITY_LIFE_STATES__",
        if has_track {
            &entity_life_states_json
        } else {
            "{}"
        },
    );
    html = html.replace("__ENTITY_OBSERVER__", "{}");
    html = html.replace(
        "__ENTITY_YAWS__",
        if has_track { &entity_yaws_json } else { "{}" },
    );
    html = html.replace(
        "__ENTITY_WEAPONS__",
        if has_track {
            &entity_weapons_json
        } else {
            "{}"
        },
    );
    html = html.replace(
        "__WEAPON_CLASSES__",
        if has_track {
            &weapon_classes_json
        } else {
            "{}"
        },
    );
    html = html.replace("__ENTITY_BUTTONS__", "{}"); // GoldSrc: no networked button stream decoded
    html = html.replace(
        "__PRIMARY_ENTITY__",
        if has_track { &primary_json } else { "null" },
    );
    html = html.replace("__VIEW_ANGLES__", &view_angles_json);
    html = html.replace("__VIEW_SWITCHES__", "[]");
    html = html.replace("__GHOST_EIDS__", "[]");
    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quake_point_budget_scales_with_duration_within_bounds() {
        assert_eq!(quake_point_budget(0.0), QUAKE_MIN_POINTS);
        assert_eq!(quake_point_budget(-5.0), QUAKE_MIN_POINTS);
        assert_eq!(quake_point_budget(1216.4), 14_596);
        assert_eq!(quake_point_budget(1.0e6), QUAKE_MAX_POINTS);
    }

    #[test]
    fn final_path_component_drops_directories() {
        assert_eq!(
            final_path_component("G:\\SteamLibrary\\steamapps\\common\\Deadlock\\game\\citadel"),
            "citadel"
        );
        assert_eq!(
            final_path_component("/opt/srcds/cs2/csgo_v2000922/csgo"),
            "csgo"
        );
        assert_eq!(final_path_component("csgo/"), "csgo");
        assert_eq!(final_path_component("dota"), "dota");
        assert_eq!(final_path_component(""), "");
    }
}
