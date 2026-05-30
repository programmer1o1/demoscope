// Multi-player position extraction using the native source_demo decoder.
// Zero external dependencies - everything lives in src/source_demo/.

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;

// Use `super::` instead of `crate::` so this resolves correctly in both the
// binary crate (where main.rs is the crate root) and the library crate
// (where lib.rs pulls main.rs in as a sub-module via #[path]).
use super::source_demo::player_tracks;
use super::source_demo::stringtable::PlayerInfo;

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
            name: p.name, steam_id: p.steam_id, user_id: p.user_id,
            is_fake: p.is_fake, is_hltv: p.is_hltv, aliases: p.aliases,
        }
    }
}

pub struct MultiPlayerData {
    // These mirror the demo header for completeness, but the main JSON output
    // pipeline gets the same info from `parse_header` so they're unread here.
    #[allow(dead_code)] pub map: String,
    #[allow(dead_code)] pub server: String,
    #[allow(dead_code)] pub duration: f32,
    #[allow(dead_code)] pub ticks: i32,
    pub tracks: HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    pub names: HashMap<u32, PlayerMeta>,
    pub life_states: HashMap<u32, Vec<(i32, u8)>>,
    pub observer_modes: HashMap<u32, Vec<(i32, u8)>>,
    pub yaws: HashMap<u32, Vec<(i32, f32, f32)>>,
    pub weapons: HashMap<u32, Vec<(i32, i32)>>,
    pub weapon_classes: HashMap<i32, String>,
    pub primary_entity: Option<u32>,
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
    let names: HashMap<u32, PlayerMeta> = raw.names.into_iter().map(|(eid, info)| (eid, PlayerMeta::from(info))).collect();
    let nick = raw.client_name.trim();
    // Match the recorder's header nick against ANY alias (signon name OR any
    // later rename). Catches both pure renames and disconnect-reconnect into
    // the same slot.
    let primary_entity = names.iter()
        .find(|(_, m)| !nick.is_empty() && m.aliases.iter().any(|a| a.trim() == nick))
        .map(|(eid, _)| *eid)
        .or_else(|| if raw.tracks.contains_key(&1) { Some(1) } else { raw.tracks.keys().min().copied() });
    Ok(MultiPlayerData {
        map: raw.map, server: raw.server, duration: raw.duration, ticks: raw.ticks,
        tracks: raw.tracks, names, life_states: raw.life_states,
        observer_modes: raw.observer_modes, yaws: raw.yaws,
        weapons: raw.weapons, weapon_classes: raw.weapon_classes,
        primary_entity,
    })
}

pub fn subsample(track: &[(i32, f32, f32, f32)], target: usize) -> Vec<(i32, f32, f32, f32)> {
    if track.len() <= target { return track.to_vec(); }
    let stride = (track.len() + target - 1) / target;
    let mut out: Vec<_> = track.iter().step_by(stride).cloned().collect();
    if let Some(last) = track.last() {
        if out.last().map(|p| p.0) != Some(last.0) { out.push(*last); }
    }
    out
}
