// Serialization of the parsed model into the JSON literals the HTML template
// interpolates, plus the small HTML escaper. Hand-rolled (no serde) to keep the
// dependency surface and wasm size down. The `multi_*` helpers project the
// shared MultiPlayerData (used by both the Source and Quake paths).

use super::super::source::events::{EventValue, GameEvent, SampledCmd};
use super::super::source::multi_player;

pub(crate) fn json_f32(v: f32) -> String {
    // Format to 3 decimal places; strip trailing zeros
    let s = format!("{:.3}", v);
    // strip trailing zeros after decimal point
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

pub(crate) fn cmds_to_json(cmds: &[SampledCmd]) -> String {
    let items: Vec<String> = cmds
        .iter()
        .map(|c| {
            format!(
                "[{},{},{},{},{},{},{}]",
                c.tick,
                json_f32(c.pitch),
                json_f32(c.yaw),
                json_f32(c.fwd),
                json_f32(c.side),
                c.btns,
                c.weapon
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

pub(crate) fn escape_json_str(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Any other control char must be \u-escaped or it produces invalid
            // JSON (the browser's JSON.parse then throws and the viewer fails to
            // load). Quake player names in particular carry raw control bytes.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn events_to_json(events: &[GameEvent]) -> String {
    let items: Vec<String> = events
        .iter()
        .map(|ev| {
            let mut fields = format!(
                "{{\"event\":\"{}\",\"tick\":{}",
                escape_json_str(&ev.event),
                ev.tick
            );
            for f in &ev.fields {
                let val_str = match &f.value {
                    EventValue::Str(s) => format!("\"{}\"", escape_json_str(s)),
                    EventValue::Float(v) => json_f32(*v),
                    EventValue::Int(v) => v.to_string(),
                    EventValue::Bool(b) => {
                        if *b {
                            "true".to_string()
                        } else {
                            "false".to_string()
                        }
                    }
                    EventValue::Null => "null".to_string(),
                };
                fields.push_str(&format!(",\"{}\":{}", escape_json_str(&f.name), val_str));
            }
            fields.push('}');
            fields
        })
        .collect();
    format!("[{}]", items.join(","))
}

pub(crate) fn breaks_to_json(breaks: &[usize]) -> String {
    format!(
        "[{}]",
        breaks
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub(crate) fn world_positions_to_json(positions: &[(i32, f32, f32, f32)]) -> String {
    let items: Vec<String> = positions
        .iter()
        .map(|(t, x, y, z)| format!("[{},{},{},{}]", t, json_f32(*x), json_f32(*y), json_f32(*z)))
        .collect();
    format!("[{}]", items.join(","))
}

pub(crate) fn meta_to_json(
    map: &str,
    client: &str,
    server: &str,
    game_dir: &str,
    demo_protocol: i32,
    duration: f32,
    ncmds: usize,
    tick_rate: f32,
    jump_threshold: f32,
) -> String {
    format!(
        "{{\"map\":\"{}\",\"client\":\"{}\",\"server\":\"{}\",\"game\":\"{}\",\"demo_protocol\":{},\"duration\":{:.2},\"ncmds\":{},\"tick_rate\":{:.2},\"jump_threshold\":{:.1}}}",
        escape_json_str(map),
        escape_json_str(client),
        escape_json_str(server),
        escape_json_str(game_dir),
        demo_protocol,
        duration,
        ncmds,
        tick_rate,
        jump_threshold,
    )
}

pub(crate) fn spawn_to_json(spawn: [f32; 3]) -> String {
    format!(
        "[{},{},{}]",
        json_f32(spawn[0]),
        json_f32(spawn[1]),
        json_f32(spawn[2])
    )
}

pub(crate) fn multi_tracks_to_json(data: &multi_player::MultiPlayerData) -> String {
    multi_tracks_to_json_with(data, DEFAULT_TRACK_POINTS)
}

/// Default per-entity point budget for position tracks.
pub(crate) const DEFAULT_TRACK_POINTS: usize = 4000;
/// Default per-entity point budget for eye-angle streams.
pub(crate) const DEFAULT_YAW_POINTS: usize = 1500;
/// Default point budget for the recorder's camera-angle stream.
pub(crate) const DEFAULT_VIEW_ANGLE_POINTS: usize = 6000;

pub(crate) fn multi_tracks_to_json_with(
    data: &multi_player::MultiPlayerData,
    target_points: usize,
) -> String {
    // Per-entity tracks → {"3":[[tick,x,y,z], ...], "4":[...], ...}
    // Subsampled to ~1500 points/entity. We include any entity that has
    // ANY track samples OR appears in life_states (so e.g. the spectator
    // entity shows up in the sidebar even with zero position updates).
    // `target_points` is the density per entity. `subsample` only thins tracks
    // longer than this, so short tracks are unaffected.
    let mut eids: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    eids.extend(data.tracks.keys().copied());
    eids.extend(data.life_states.keys().copied());
    eids.extend(data.names.keys().copied());

    let mut entries: Vec<String> = eids
        .into_iter()
        .map(|eid| {
            let empty = Vec::new();
            let samples = data.tracks.get(&eid).unwrap_or(&empty);
            let reduced = multi_player::subsample(samples, target_points);
            let pts: Vec<String> = reduced
                .iter()
                .map(|(t, x, y, z)| {
                    format!("[{},{},{},{}]", t, json_f32(*x), json_f32(*y), json_f32(*z))
                })
                .collect();
            format!("\"{}\":[{}]", eid, pts.join(","))
        })
        .collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

pub(crate) fn multi_life_states_to_json(data: &multi_player::MultiPlayerData) -> String {
    let mut entries: Vec<String> = data
        .life_states
        .iter()
        .map(|(eid, states)| {
            let pts: Vec<String> = states
                .iter()
                .map(|(t, s)| format!("[{},{}]", t, s))
                .collect();
            format!("\"{}\":[{}]", eid, pts.join(","))
        })
        .collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

pub(crate) fn multi_observer_modes_to_json(data: &multi_player::MultiPlayerData) -> String {
    // Per-entity observer-mode transitions: entity_id → [[tick, mode], ...].
    // mode 0 = playing; non-zero = spectating. Same shape as life-states.
    let mut entries: Vec<String> = data
        .observer_modes
        .iter()
        .map(|(eid, states)| {
            let pts: Vec<String> = states
                .iter()
                .map(|(t, m)| format!("[{},{}]", t, m))
                .collect();
            format!("\"{}\":[{}]", eid, pts.join(","))
        })
        .collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

pub(crate) fn multi_weapons_to_json(data: &multi_player::MultiPlayerData) -> String {
    // Per-player active-weapon stream: entity_id → [[tick, weapon_eid], ...].
    let mut entries: Vec<String> = data
        .weapons
        .iter()
        .map(|(eid, w)| {
            let pts: Vec<String> = w
                .iter()
                .map(|(t, wid)| format!("[{},{}]", t, wid))
                .collect();
            format!("\"{}\":[{}]", eid, pts.join(","))
        })
        .collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

pub(crate) fn multi_weapon_classes_to_json(data: &multi_player::MultiPlayerData) -> String {
    // weapon_eid → class name; strip the "CTF" prefix and lowercase to look
    // tidy in the UI (`CTFRocketLauncher` → `rocketlauncher`).
    let mut entries: Vec<String> = data
        .weapon_classes
        .iter()
        .map(|(eid, name)| {
            let trimmed = name
                .strip_prefix("CTFWeapon")
                .or_else(|| name.strip_prefix("CTF"))
                .unwrap_or(name);
            format!(
                "\"{}\":\"{}\"",
                eid,
                escape_json_str(&trimmed.to_lowercase())
            )
        })
        .collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

pub(crate) fn multi_yaws_to_json(data: &multi_player::MultiPlayerData) -> String {
    multi_yaws_to_json_with(data, DEFAULT_YAW_POINTS)
}

pub(crate) fn multi_yaws_to_json_with(
    data: &multi_player::MultiPlayerData,
    target: usize,
) -> String {
    // Per-entity eye-angle yaw stream subsampled to a manageable size. Used by
    // the input panel to project velocity into the player's local frame for
    // proper WSAD reconstruction. Drop deg-fractions to 1 decimal to shave
    // bytes since the panel rounds to 90° quadrants anyway.
    let mut entries: Vec<String> = data
        .yaws
        .iter()
        .map(|(eid, ys)| {
            let stride = ys.len().div_ceil(target).max(1);
            // [tick, yaw, pitch] - pitch (3rd element) drives the first-person
            // camera on proto-4 demos; older JS consumers read only [0]/[1].
            let mut pts: Vec<String> = ys
                .iter()
                .step_by(stride)
                .map(|(t, y, p)| format!("[{},{:.1},{:.1}]", t, y, p))
                .collect();
            if stride > 1 {
                if let Some(last) = ys.last() {
                    pts.push(format!("[{},{:.1},{:.1}]", last.0, last.1, last.2));
                }
            }
            format!("\"{}\":[{}]", eid, pts.join(","))
        })
        .collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

pub(crate) fn view_angles_to_json(data: &multi_player::MultiPlayerData) -> String {
    view_angles_to_json_with(data, DEFAULT_VIEW_ANGLE_POINTS)
}

pub(crate) fn view_angles_to_json_with(
    data: &multi_player::MultiPlayerData,
    target: usize,
) -> String {
    // Recorder's per-frame camera angles [tick, pitch, yaw] from democmdinfo.
    // Dense (~1 per game packet) and authoritative - drives the FPS camera on
    // demos with no usercmds. Keep generous resolution; this is the whole point.
    let v = &data.view_angles;
    let stride = v.len().div_ceil(target).max(1);
    let mut pts: Vec<String> = v
        .iter()
        .step_by(stride)
        .map(|(t, p, y)| format!("[{},{:.2},{:.2}]", t, p, y))
        .collect();
    if stride > 1 {
        if let Some(last) = v.last() {
            pts.push(format!("[{},{:.2},{:.2}]", last.0, last.1, last.2));
        }
    }
    format!("[{}]", pts.join(","))
}

pub(crate) fn multi_names_to_json(data: &multi_player::MultiPlayerData) -> String {
    let mut entries: Vec<String> = data.names.iter().map(|(eid, meta)| {
        let aliases: Vec<String> = meta.aliases.iter()
            .map(|a| format!("\"{}\"", escape_json_str(a))).collect();
        // Economy + scoreboard (CS:S / CS:GO). Emitted on every player so the
        // viewer can render a scoreboard panel; all-zero on non-CS demos.
        let e = data.econ.get(eid).cloned().unwrap_or_default();
        format!(
            "\"{}\":{{\"name\":\"{}\",\"steam_id\":\"{}\",\"user_id\":{},\"is_fake\":{},\"is_hltv\":{},\"aliases\":[{}],\"kills\":{},\"deaths\":{},\"assists\":{},\"money\":{},\"score\":{},\"mvps\":{},\"team\":{}}}",
            eid,
            escape_json_str(&meta.name),
            escape_json_str(&meta.steam_id),
            meta.user_id,
            meta.is_fake,
            meta.is_hltv,
            aliases.join(","),
            e.kills, e.deaths, e.assists, e.money, e.score, e.mvps, e.team,
        )
    }).collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

pub(crate) fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
