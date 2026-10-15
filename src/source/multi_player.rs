// Multi-player position extraction using the native source_demo decoder.
// Zero external dependencies - everything lives in src/source_demo/.

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;

// Use `super::` instead of `crate::` so this resolves correctly in both the
// binary crate (where main.rs is the crate root) and the library crate
// (where lib.rs pulls main.rs in as a sub-module via #[path]).
use super::player_tracks::{self, PlayerEcon};
use super::stringtable::PlayerInfo;

#[derive(Default)]
pub struct PlayerMeta {
    pub name: String,
    pub steam_id: String,
    pub user_id: u32,
    pub is_fake: bool,
    pub is_hltv: bool,
    pub aliases: Vec<String>,
}

impl From<PlayerInfo> for PlayerMeta {
    fn from(p: PlayerInfo) -> Self {
        PlayerMeta {
            name: p.name,
            steam_id: p.steam_id,
            user_id: p.user_id,
            is_fake: p.is_fake,
            is_hltv: p.is_hltv,
            aliases: p.aliases,
        }
    }
}

pub struct MultiPlayerData {
    // These mirror the demo header for completeness, but the main JSON output
    // pipeline gets the same info from `parse_header` so they're unread here.
    #[allow(dead_code)]
    pub map: String,
    #[allow(dead_code)]
    pub server: String,
    #[allow(dead_code)]
    pub duration: f32,
    #[allow(dead_code)]
    pub ticks: i32,
    pub tracks: HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    pub names: HashMap<u32, PlayerMeta>,
    pub life_states: HashMap<u32, Vec<(i32, u8)>>,
    pub observer_modes: HashMap<u32, Vec<(i32, u8)>>,
    pub yaws: HashMap<u32, Vec<(i32, f32, f32)>>,
    pub weapons: HashMap<u32, Vec<(i32, i32)>>,
    pub weapon_classes: HashMap<i32, String>,
    /// Per-player economy + scoreboard (CS:S / CS:GO), keyed by player entity id
    /// to line up with `names`. Empty on non-CS demos.
    pub econ: HashMap<u32, PlayerEcon>,
    pub primary_entity: Option<u32>,
    /// Recorder's per-frame camera angles (tick, pitch, yaw) from democmdinfo.
    pub view_angles: Vec<(i32, f32, f32)>,
}

#[allow(dead_code)] // CLI flow now reads bytes up front and calls extract_from_bytes.
pub fn extract(dem_path: &Path) -> Result<MultiPlayerData, Box<dyn Error>> {
    let raw = player_tracks::extract(dem_path)?;
    wrap_raw(raw)
}

#[allow(dead_code)] // wired up by the WASM entry point in lib.rs
pub fn extract_from_bytes(bytes: &[u8]) -> Result<MultiPlayerData, Box<dyn Error>> {
    let raw = player_tracks::extract_from_bytes(bytes)?;
    wrap_raw(raw)
}

fn wrap_raw(raw: player_tracks::PlayerTrackData) -> Result<MultiPlayerData, Box<dyn Error>> {
    let names: HashMap<u32, PlayerMeta> = raw
        .names
        .into_iter()
        .map(|(eid, info)| (eid, PlayerMeta::from(info)))
        .collect();
    let nick = raw.client_name.trim();
    // Match the recorder's header nick against ANY alias (signon name OR any
    // later rename). Catches both pure renames and disconnect-reconnect into
    // the same slot.
    let primary_entity = names
        .iter()
        .find(|(_, m)| !nick.is_empty() && m.aliases.iter().any(|a| a.trim() == nick))
        .map(|(eid, _)| *eid)
        .or_else(|| {
            if raw.tracks.contains_key(&1) {
                Some(1)
            } else {
                raw.tracks.keys().min().copied()
            }
        });
    Ok(MultiPlayerData {
        map: raw.map,
        server: raw.server,
        duration: raw.duration,
        ticks: raw.ticks,
        tracks: raw.tracks,
        names,
        life_states: raw.life_states,
        observer_modes: raw.observer_modes,
        yaws: raw.yaws,
        weapons: raw.weapons,
        weapon_classes: raw.weapon_classes,
        econ: raw.econ,
        primary_entity,
        view_angles: raw.view_angles,
    })
}

// A single raw step longer than this is a teleport or respawn, not movement.
const JUMP_KEEP_DIST_SQ: f32 = 200.0 * 200.0;

/// Thins a track to roughly `target` points by keeping every Nth sample, plus
/// both ends of every large single-step jump. Without those, a teleport between
/// two strided samples looks like fast-but-plausible movement, so the viewer
/// joins the two places with a line and counts it as a speed peak.
pub fn subsample(track: &[(i32, f32, f32, f32)], target: usize) -> Vec<(i32, f32, f32, f32)> {
    if track.len() <= target {
        return track.to_vec();
    }
    let stride = track.len().div_ceil(target);
    let mut keep = vec![false; track.len()];
    for i in (0..track.len()).step_by(stride) {
        keep[i] = true;
    }
    keep[track.len() - 1] = true;

    let jumps: Vec<usize> = (1..track.len())
        .filter(|&i| {
            let (_, ax, ay, az) = track[i - 1];
            let (_, bx, by, bz) = track[i];
            let (dx, dy, dz) = (bx - ax, by - ay, bz - az);
            dx * dx + dy * dy + dz * dz > JUMP_KEEP_DIST_SQ
        })
        .collect();
    // A track that "jumps" constantly is sparse rather than teleporting; keep
    // the plain stride so the output size stays bounded.
    if jumps.len() <= target / 2 {
        for i in jumps {
            keep[i - 1] = true;
            keep[i] = true;
        }
    }

    track
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::subsample;

    #[test]
    fn subsample_keeps_both_ends_of_a_teleport() {
        // 1000 samples walking 5u per tick, with a 900u teleport at index 503.
        let track: Vec<_> = (0..1000)
            .map(|i| {
                let x = i as f32 * 5.0 + if i >= 503 { 900.0 } else { 0.0 };
                (i, x, 0.0, 0.0)
            })
            .collect();
        let out = subsample(&track, 100);
        assert!(out.len() <= 103, "{}", out.len());
        assert!(out.contains(&track[502]) && out.contains(&track[503]));
        assert_eq!(out.first(), track.first());
        assert_eq!(out.last(), track.last());
        assert!(out.windows(2).all(|w| w[0].0 < w[1].0), "stays in order");
    }

    #[test]
    fn subsample_leaves_short_tracks_alone() {
        let track = vec![(0, 0.0, 0.0, 0.0), (1, 999.0, 0.0, 0.0)];
        assert_eq!(subsample(&track, 100), track);
    }
}
