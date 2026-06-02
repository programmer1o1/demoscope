// Quake-family demo decoding (Quake 1 / 2 / 3).
//
// These are a different lineage from the Source `HL2DEMO` format the rest of
// the crate decodes, but they slot into the SAME output: each parser produces a
// `MultiPlayerData` (per-entity position/angle tracks + names) which the
// existing HTML viewer renders. Zero external dependencies, same as source_demo.
//
// Why these are tractable where Source was hard: Quake demos are recordings of
// the server's network stream, so entity *positions are already in the bytes* -
// we read them, we don't re-simulate. Q1/Q2 are plain byte/short oriented; Q3
// adds a static-Huffman bitstream but is otherwise the same shape.

use std::collections::HashMap;
use std::error::Error;

use super::multi_player::{MultiPlayerData, PlayerMeta};

pub mod q1;
pub mod q2;
pub mod q3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuakeKind {
    Q1Net, // NetQuake .dem (protocol 15)
    QwMvd, // QuakeWorld demo / multiview (.qwd / .mvd)
    Q2,    // .dm2 (protocol 34)
    Q3,    // .dm_6x (protocol 66/67/68)
}

pub struct QuakeMeta {
    pub map: String,
    pub server: String,
    pub client: String,
    pub game: String, // "quake1" / "quake2" / "quake3"
    pub protocol: i32,
    pub duration: f32,
    pub tick_rate: f32,
    pub ncmds: usize,
}

pub struct QuakeDemo {
    pub meta: QuakeMeta,
    pub mpd: MultiPlayerData,
}

/// Decide whether a file is a Quake-family demo (and which), from its name and
/// leading bytes. Returns None for Source demos (HL2DEMO magic) and unknowns,
/// so the caller falls back to the normal Source path.
pub fn detect(name: &str, data: &[u8]) -> Option<QuakeKind> {
    // Source demos carry an explicit magic - never claim those. Garry's Mod
    // 13+ uses the identical HL2DEMO container under a renamed `GMODEMO` magic;
    // exclude it too, otherwise its `.dem` extension would be misrouted here as
    // a NetQuake demo (the `.dem` fallthrough below).
    if data.len() >= 8 && (&data[0..8] == b"HL2DEMO\0" || &data[0..8] == b"GMODEMO\0") {
        return None;
    }
    // GoldSrc (HL1) HLDEMO demos also use the `.dem` extension but are handled
    // by the dedicated goldsrc module - don't claim them as NetQuake.
    if data.len() >= 8 && &data[0..8] == b"HLDEMO\0\0" {
        return None;
    }
    let lname = name.to_lowercase();
    if lname.contains(".dm_") {
        return Some(QuakeKind::Q3); // .dm_68 / .dm_67 / .dm_66 ...
    }
    if lname.ends_with(".dm2") {
        return Some(QuakeKind::Q2);
    }
    if lname.ends_with(".mvd") || lname.ends_with(".qwd") {
        return Some(QuakeKind::QwMvd);
    }
    if lname.ends_with(".dem") {
        // A .dem that is NOT HL2DEMO is a NetQuake demo.
        return Some(QuakeKind::Q1Net);
    }
    None
}

pub fn parse(kind: QuakeKind, data: &[u8], name: &str) -> Result<QuakeDemo, Box<dyn Error>> {
    match kind {
        QuakeKind::Q2 => q2::parse(data),
        QuakeKind::Q3 => q3::parse(data, name),
        QuakeKind::Q1Net => q1::parse_netquake(data),
        QuakeKind::QwMvd => q1::parse_qw_mvd(data),
    }
}

/// Extract the protocol number from a Quake 3 demo extension (`foo.dm_68` → 68).
pub fn dm_protocol(name: &str) -> Option<i32> {
    let l = name.to_lowercase();
    let idx = l.rfind(".dm_")?;
    l[idx + 4..].trim().parse::<i32>().ok()
}

// ── Shared building block: a bounds-checked little-endian byte reader ──────────
//
// Mirrors the "overflow-hardened" style of the Source walkers: reads past the
// end return 0/empty and latch `overflow`, so a parse loop checks `r.overflow`
// and stops cleanly instead of panicking on a malformed/truncated demo.
pub struct ByteReader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    pub overflow: bool,
}

impl<'a> ByteReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        ByteReader { data, pos: 0, overflow: false }
    }
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    pub fn eof(&self) -> bool {
        self.pos >= self.data.len()
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.pos + n > self.data.len() {
            self.overflow = true;
            self.pos = self.data.len();
            return None;
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }
    pub fn skip(&mut self, n: usize) {
        let _ = self.take(n);
    }
    pub fn read_u8(&mut self) -> u8 {
        self.take(1).map(|b| b[0]).unwrap_or(0)
    }
    pub fn read_i8(&mut self) -> i8 {
        self.read_u8() as i8
    }
    pub fn read_u16(&mut self) -> u16 {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0)
    }
    pub fn read_i16(&mut self) -> i16 {
        self.read_u16() as i16
    }
    pub fn read_u32(&mut self) -> u32 {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0)
    }
    pub fn read_i32(&mut self) -> i32 {
        self.read_u32() as i32
    }
    pub fn read_f32(&mut self) -> f32 {
        f32::from_bits(self.read_u32())
    }
    pub fn read_bytes(&mut self, n: usize) -> Vec<u8> {
        self.take(n).map(|b| b.to_vec()).unwrap_or_default()
    }
    /// Read a NUL-terminated string (Quake uses C strings on the wire) and
    /// "de-quake" it to readable ASCII. Quake renders text with the high bit
    /// set for the coloured/bronze font and uses bytes 0x12-0x1b as the gold
    /// digit font, so a raw read gives garbage like "\x13\x13\x15\x16?Turmoil"
    /// (which is really "1134 Turmoil"). We strip the colour bit, map the gold
    /// digits and bracket glyphs back, and drop the remaining control glyphs.
    pub fn read_string(&mut self) -> String {
        let mut out = String::new();
        loop {
            if self.eof() {
                break;
            }
            let c = self.read_u8();
            if c == 0 {
                break;
            }
            let low = c & 0x7f; // strip colour / high-bit
            match low {
                0x12..=0x1b => out.push((b'0' + (low - 0x12)) as char), // gold digits
                0x10 => out.push('['),
                0x11 => out.push(']'),
                0x20..=0x7e => out.push(low as char), // printable ASCII
                _ => {}                               // drop other font glyphs
            }
        }
        out
    }

    // ── Quake value helpers ──
    /// Q1/Q2 entity origin coordinate: a 1/8-unit fixed-point short.
    pub fn read_coord_q2(&mut self) -> f32 {
        self.read_i16() as f32 * 0.125
    }
    /// 8-bit angle (entity angles): byte mapped to [0,360).
    pub fn read_angle8(&mut self) -> f32 {
        self.read_u8() as f32 * (360.0 / 256.0)
    }
    /// 16-bit angle (player viewangles): short mapped to [0,360).
    pub fn read_angle16(&mut self) -> f32 {
        self.read_u16() as f32 * (360.0 / 65536.0)
    }
}

// ── MultiPlayerData assembly ───────────────────────────────────────────────────

/// Accumulates per-entity samples during a parse, then bakes a MultiPlayerData.
#[derive(Default)]
pub struct TrackBuilder {
    pub tracks: HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    pub yaws: HashMap<u32, Vec<(i32, f32, f32)>>, // (tick, yaw, pitch)
    pub names: HashMap<u32, String>,
    pub primary: Option<u32>,
    pub view_angles: Vec<(i32, f32, f32)>, // (tick, pitch, yaw) - recorder POV
    // (tick, state) transitions; 0 = alive/playing, non-zero = dead/spectating.
    // The viewer hides an avatar wherever it's dead or spectating.
    pub life_states: HashMap<u32, Vec<(i32, u8)>>,
    pub observer_modes: HashMap<u32, Vec<(i32, u8)>>,
    last_life: HashMap<u32, u8>,
    last_obs: HashMap<u32, u8>,
}

impl TrackBuilder {
    pub fn pos(&mut self, eid: u32, tick: i32, x: f32, y: f32, z: f32) {
        self.tracks.entry(eid).or_default().push((tick, x, y, z));
    }
    pub fn yaw(&mut self, eid: u32, tick: i32, yaw: f32, pitch: f32) {
        self.yaws.entry(eid).or_default().push((tick, yaw, pitch));
    }
    pub fn name(&mut self, eid: u32, name: String) {
        if !name.is_empty() {
            self.names.insert(eid, name);
        }
    }
    /// Record a death/alive transition (only emitted when the state flips, so
    /// the stream stays compact). `dead` hides the avatar during that window.
    pub fn life(&mut self, eid: u32, tick: i32, dead: bool) {
        let v = u8::from(dead);
        if self.last_life.get(&eid) == Some(&v) {
            return;
        }
        self.last_life.insert(eid, v);
        self.life_states.entry(eid).or_default().push((tick, v));
    }
    /// Record a spectating/playing transition (only on change).
    pub fn observe(&mut self, eid: u32, tick: i32, spectating: bool) {
        let v = u8::from(spectating);
        if self.last_obs.get(&eid) == Some(&v) {
            return;
        }
        self.last_obs.insert(eid, v);
        self.observer_modes.entry(eid).or_default().push((tick, v));
    }

    pub fn build(self, map: String, server: String, duration: f32, ticks: i32) -> MultiPlayerData {
        let names: HashMap<u32, PlayerMeta> = self
            .names
            .into_iter()
            .map(|(eid, n)| {
                (
                    eid,
                    PlayerMeta {
                        name: n.clone(),
                        steam_id: String::new(),
                        user_id: eid,
                        is_fake: false,
                        is_hltv: false,
                        aliases: vec![n],
                    },
                )
            })
            .collect();
        let primary = self
            .primary
            .or_else(|| self.tracks.keys().min().copied());
        MultiPlayerData {
            map,
            server,
            duration,
            ticks,
            tracks: self.tracks,
            names,
            life_states: self.life_states,
            observer_modes: self.observer_modes,
            yaws: self.yaws,
            weapons: HashMap::new(),
            weapon_classes: HashMap::new(),
            primary_entity: primary,
            view_angles: self.view_angles,
        }
    }
}
