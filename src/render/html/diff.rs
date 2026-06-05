// Diff-overlay helpers: decode a second demo into translucent "ghost" tracks
// (or its recorder path as a single ghost), and the small JSON-object merge
// used to splice those ghosts into the host scene's track maps. Consumed by the
// Source generator in the parent `html` module.

use super::super::super::source::multi_player;
use super::super::super::header::{parse_header, parse_usercmd};
use super::super::super::source::packets::detect_splitscreen;
use super::super::super::util::bytes::{le_f32, le_i32};
use super::super::super::util::constants::{HEADER_SIZE, SPLIT_SIZE};

/// Insert `"key":value` into a JSON object string (`{...}` or `{}`).
pub(super) fn merge_json_obj(base: &str, key: u32, value: &str) -> String {
    let t = base.trim();
    if t.len() <= 2 {
        format!("{{\"{}\":{}}}", key, value)
    } else {
        format!("{{\"{}\":{},{}", key, value, &t[1..])
    }
}

/// Walk a Source demo's democmdinfo `viewOrigin` + usercmd view angles to get
/// the recorder's run path. Used as the single-POV fallback when a diffed demo
/// has no networked entities (Portal/HL2 SP). Returns (pos[tick,x,y,z],
/// yaws[tick,yaw,pitch]).
fn extract_recorder_path(data: &[u8]) -> (Vec<(i32, f32, f32, f32)>, Vec<(i32, f32, f32)>) {
    let mut positions = Vec::new();
    let mut yaws = Vec::new();
    let header = match parse_header(data) {
        Some(h) => h,
        None => return (positions, yaws),
    };
    if header.net_protocol <= 7 {
        return (positions, yaws);
    }
    let proto = header.demo_protocol;
    let extra: usize = if proto > 3 { 1 } else { 0 };
    let pkt_hdr = 5 + extra;
    let democmdinfo = SPLIT_SIZE * detect_splitscreen(data, proto, &header.game_dir);
    let (mut last_pitch, mut last_yaw) = (0.0f32, 0.0f32);
    let mut offset = HEADER_SIZE;
    while offset < data.len() {
        if offset + 5 > data.len() {
            break;
        }
        let cmd = data[offset];
        let tick = le_i32(data, offset + 1);
        match cmd {
            7 => break,
            1 | 2 => {
                let p = offset + pkt_hdr;
                if p + democmdinfo + 12 > data.len() {
                    break;
                }
                if cmd == 2 && p + 16 <= data.len() {
                    let x = le_f32(data, p + 4);
                    let y = le_f32(data, p + 8);
                    let z = le_f32(data, p + 12);
                    if x != 0.0 || y != 0.0 || z != 0.0 {
                        positions.push((tick, x, y, z));
                    }
                }
                let length = le_i32(data, p + democmdinfo + 8);
                if length < 0 {
                    break;
                }
                offset = p.saturating_add(democmdinfo + 12).saturating_add(length as usize);
            }
            3 => offset += pkt_hdr,
            4 => {
                let p = offset + pkt_hdr;
                if p + 4 > data.len() {
                    break;
                }
                let length = le_i32(data, p);
                if length < 0 {
                    break;
                }
                offset = p.saturating_add(4).saturating_add(length as usize);
            }
            5 => {
                let p = offset + pkt_hdr;
                if p + 8 > data.len() {
                    break;
                }
                let length = le_i32(data, p + 4);
                if length < 0 {
                    break;
                }
                let next = p.saturating_add(8).saturating_add(length as usize);
                if next > data.len() {
                    break;
                }
                if let Some(uc) = parse_usercmd(&data[p + 8..next]) {
                    if let Some(pi) = uc.pitch {
                        last_pitch = pi;
                    }
                    if let Some(ya) = uc.yaw {
                        last_yaw = ya;
                    }
                    yaws.push((tick, last_yaw, last_pitch));
                }
                offset = next;
            }
            6 | 8 | 9 => {
                let p = offset + pkt_hdr;
                if p + 4 > data.len() {
                    break;
                }
                let length = le_i32(data, p);
                if length < 0 {
                    break;
                }
                offset = p
                    .saturating_add(4)
                    .saturating_add(length as usize)
                    .min(data.len());
            }
            _ => break,
        }
    }
    (positions, yaws)
}

/// One overlaid entity from a diffed second demo.
pub(super) struct Ghost {
    pub(super) eid: u32,
    pub(super) name: String,
    pub(super) samples: Vec<(i32, f32, f32, f32)>,
    pub(super) yaws: Vec<(i32, f32, f32)>,
    pub(super) life: Vec<(i32, u8)>,
}

/// Decode a diffed demo into ghosts: every networked entity track (multiplayer),
/// or the recorder camera path as a single ghost (single-POV). Ghost eids are
/// offset by 900000 so they never collide with the host demo's entity ids.
pub(super) fn extract_ghosts(d2: &[u8], d2name: &str) -> Vec<Ghost> {
    const BASE: u32 = 900_000;
    let mut out = Vec::new();
    if let Ok(mpd) = multi_player::extract_from_bytes(d2) {
        let mut eids: Vec<u32> = mpd.tracks.keys().copied().collect();
        eids.sort_unstable();
        for eid in eids {
            let track = &mpd.tracks[&eid];
            if track.len() < 2 {
                continue;
            }
            let nm = mpd
                .names
                .get(&eid)
                .map(|m| m.name.clone())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("e{}", eid));
            out.push(Ghost {
                eid: BASE + eid,
                name: format!("\u{25c6} {}", nm),
                samples: track.clone(),
                yaws: mpd.yaws.get(&eid).cloned().unwrap_or_default(),
                life: mpd.life_states.get(&eid).cloned().unwrap_or_default(),
            });
        }
    }
    if out.is_empty() {
        // Single-POV demo: overlay the recorder camera path as one ghost.
        let (pos, yaw) = extract_recorder_path(d2);
        if pos.len() >= 2 {
            out.push(Ghost {
                eid: BASE + 1,
                name: format!("\u{25c6} {}", d2name),
                samples: pos,
                yaws: yaw,
                life: Vec::new(),
            });
        }
    }
    out
}

