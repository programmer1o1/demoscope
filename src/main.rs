// This file is both the CLI binary entry point AND (via `#[path]`-include in
// lib.rs) the implementation sub-module for the wasm library. Many constants
// and helpers are reached only through `fn main()`, which the library build
// doesn't treat as a root entry point - so the dead-code lint fires across
// the board even though everything is used in the CLI path. Module-level
// allows mute that noise without affecting the CLI's analysis.
#![allow(dead_code, unused_imports)]

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::File;
use std::io::{self, Read, Write as IoWrite};
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use lzma_rs::lzma_decompress;

mod source_demo;
mod multi_player;

// ─── Constants ───────────────────────────────────────────────────────────────

const DEMO_MAGIC: &[u8; 8] = b"HL2DEMO\0";
const HEADER_SIZE: usize = 1072;
const DEMOCMDINFO_SIZE: usize = 76; // flags(4) + 6 × vec3(12) = 76

// Packet command IDs
const DEM_SIGNON: u8 = 1;
const DEM_PACKET: u8 = 2;
const DEM_SYNCTICK: u8 = 3;
const DEM_CONSOLECMD: u8 = 4;
const DEM_USERCMD: u8 = 5;
const DEM_DATATABLES: u8 = 6;
const DEM_STOP: u8 = 7;
const DEM_STRINGTABLES: u8 = 8;


// MAX_EDICT_BITS / WEAPON_SUBTYPE_BITS from Source SDK
const MAX_EDICT_BITS: u32 = 11;
const WEAPON_SUBTYPE_BITS: u32 = 6;

// IN_* button masks
const IN_ATTACK: u32 = 1 << 0;
const IN_JUMP: u32 = 1 << 1;
const IN_DUCK: u32 = 1 << 2;
const IN_FORWARD: u32 = 1 << 3;
const IN_BACK: u32 = 1 << 4;
const IN_USE: u32 = 1 << 5;
const IN_LEFT: u32 = 1 << 7;
const IN_RIGHT: u32 = 1 << 8;
const IN_MOVELEFT: u32 = 1 << 9;
const IN_MOVERIGHT: u32 = 1 << 10;
const IN_ATTACK2: u32 = 1 << 11;
const IN_RELOAD: u32 = 1 << 13;
const IN_SCORE: u32 = 1 << 16;
const IN_SPEED: u32 = 1 << 17;
const IN_WALK: u32 = 1 << 18;
const IN_ZOOM: u32 = 1 << 19;


// ─── Data structures ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct DemoHeader {
    demo_protocol: i32,
    net_protocol: i32,
    server_name: String,
    client_name: String,
    map_name: String,
    game_dir: String,
    playback_time: f32,
    ticks: i32,
    frames: i32,
    sign_on_length: i32,
}

#[derive(Debug, Default)]
struct UserCmd {
    command_number: Option<u32>,
    tick_count: Option<u32>,
    pitch: Option<f32>,
    yaw: Option<f32>,
    roll: Option<f32>,
    forwardmove: Option<f32>,
    sidemove: Option<f32>,
    upmove: Option<f32>,
    buttons: Option<u32>,
    impulse: Option<u8>,
    weaponselect: Option<u32>,
    weaponsubtype: Option<u32>,
    mousedx: Option<i16>,
    mousedy: Option<i16>,
}


// ─── Source Engine CBitBuf reader (LSB-first within each byte) ───────────────

struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
    max_bit: usize,  // hard upper limit; defaults to data.len()*8
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        let max = data.len() * 8;
        BitReader { data, bit_pos: 0, max_bit: max }
    }

    fn new_at(data: &'a [u8], pos: usize) -> Self {
        let max = data.len() * 8;
        BitReader { data, bit_pos: pos, max_bit: max }
    }

    fn read_bits(&mut self, n: u32) -> Option<u32> {
        if self.max_bit < self.bit_pos + n as usize {
            return None;
        }
        let mut result = 0u32;
        for i in 0..n {
            let byte_idx = self.bit_pos / 8;
            let bit_idx = self.bit_pos % 8;
            let bit = ((self.data[byte_idx] >> bit_idx) & 1) as u32;
            result |= bit << i;
            self.bit_pos += 1;
        }
        Some(result)
    }

    fn read_u32(&mut self) -> Option<u32> {
        self.read_bits(32)
    }

    // WriteBitFloat stores the raw IEEE-754 bits.
    fn read_bit_float(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.read_u32()?))
    }

    fn read_i16(&mut self) -> Option<i16> {
        Some(self.read_bits(16)? as i16)
    }

    fn read_byte(&mut self) -> Option<u8> {
        Some(self.read_bits(8)? as u8)
    }

    fn skip(&mut self, n: u32) -> bool {
        if self.max_bit < self.bit_pos + n as usize {
            return false;
        }
        self.bit_pos += n as usize;
        true
    }

    fn bits_remaining(&self) -> usize {
        let total = self.data.len() * 8;
        if total > self.bit_pos { total - self.bit_pos } else { 0 }
    }

    fn try_read_cstring(&mut self, max: usize) -> Option<String> {
        let mut chars = Vec::new();
        for _ in 0..max {
            if self.bits_remaining() < 8 {
                return None;
            }
            let b = self.read_bits(8)? as u8;
            if b == 0 {
                break;
            }
            // Must be ASCII alphanumeric or underscore
            if !b.is_ascii_alphanumeric() && b != b'_' {
                return None;
            }
            chars.push(b);
        }
        Some(String::from_utf8(chars).ok()?)
    }

    fn read_cstring_any(&mut self, max: usize) -> Option<String> {
        let mut chars = Vec::new();
        for _ in 0..max {
            if self.bits_remaining() < 8 {
                return None;
            }
            let b = self.read_bits(8)? as u8;
            if b == 0 {
                break;
            }
            chars.push(b);
        }
        Some(String::from_utf8_lossy(&chars).into_owned())
    }
}

// ─── Byte helpers ─────────────────────────────────────────────────────────────

fn le_i32(data: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

fn le_f32(data: &[u8], off: usize) -> f32 {
    f32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

fn le_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(data[off..off + 2].try_into().unwrap())
}

fn le_i16_bytes(data: &[u8], off: usize) -> i16 {
    i16::from_le_bytes(data[off..off + 2].try_into().unwrap())
}

fn read_cstring(data: &[u8], off: usize, max: usize) -> String {
    let end = data[off..off + max]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(max);
    String::from_utf8_lossy(&data[off..off + end]).into_owned()
}

// ─── Parsers ─────────────────────────────────────────────────────────────────

fn parse_header(data: &[u8]) -> Option<DemoHeader> {
    if data.len() < HEADER_SIZE {
        return None;
    }
    if &data[0..8] != DEMO_MAGIC {
        return None;
    }
    Some(DemoHeader {
        demo_protocol: le_i32(data, 8),
        net_protocol: le_i32(data, 12),
        server_name: read_cstring(data, 16, 260),
        client_name: read_cstring(data, 276, 260),
        map_name: read_cstring(data, 536, 260),
        game_dir: read_cstring(data, 796, 260),
        playback_time: le_f32(data, 1056),
        ticks: le_i32(data, 1060),
        frames: le_i32(data, 1064),
        sign_on_length: le_i32(data, 1068),
    })
}

// Parses a CUserCmd payload using Source Engine ReadUsercmd() format.
// Each field is preceded by a 1-bit "has this field" flag in the CBitBuf stream.
// Returns partial ucmd on buffer exhaustion rather than failing entirely.
fn parse_usercmd(data: &[u8]) -> Option<UserCmd> {
    let mut br = BitReader::new(data);
    let mut ucmd = UserCmd::default();

    macro_rules! has {
        () => {
            match br.read_bits(1) {
                Some(v) => v != 0,
                None => return Some(ucmd),
            }
        };
    }

    if has!() { ucmd.command_number = br.read_u32(); }
    if has!() { ucmd.tick_count    = br.read_u32(); }
    if has!() { ucmd.pitch          = br.read_bit_float(); }
    if has!() { ucmd.yaw            = br.read_bit_float(); }
    if has!() { ucmd.roll           = br.read_bit_float(); }
    if has!() { ucmd.forwardmove    = br.read_bit_float(); }
    if has!() { ucmd.sidemove       = br.read_bit_float(); }
    if has!() { ucmd.upmove         = br.read_bit_float(); }
    if has!() { ucmd.buttons        = br.read_u32(); }
    if has!() { ucmd.impulse        = br.read_byte(); }
    if has!() {
        ucmd.weaponselect = br.read_bits(MAX_EDICT_BITS);
        if has!() { ucmd.weaponsubtype = br.read_bits(WEAPON_SUBTYPE_BITS); }
    }
    if has!() { ucmd.mousedx = br.read_i16(); }
    if has!() { ucmd.mousedy = br.read_i16(); }

    Some(ucmd)
}

// ─── Display helpers ──────────────────────────────────────────────────────────

fn fmt_buttons(b: u32) -> String {
    const NAMES: &[(u32, &str)] = &[
        (IN_ATTACK, "ATTACK"),
        (IN_ATTACK2, "ATTACK2"),
        (IN_JUMP, "JUMP"),
        (IN_DUCK, "DUCK"),
        (IN_FORWARD, "FORWARD"),
        (IN_BACK, "BACK"),
        (IN_MOVELEFT, "MOVELEFT"),
        (IN_MOVERIGHT, "MOVERIGHT"),
        (IN_USE, "USE"),
        (IN_RELOAD, "RELOAD"),
        (IN_SCORE, "SCORE"),
        (IN_SPEED, "SPEED"),
        (IN_WALK, "WALK"),
        (IN_ZOOM, "ZOOM"),
        (IN_LEFT, "TURNLEFT"),
        (IN_RIGHT, "TURNRIGHT"),
    ];
    let active: Vec<&str> = NAMES
        .iter()
        .filter(|(f, _)| b & f != 0)
        .map(|(_, n)| *n)
        .collect();
    if active.is_empty() {
        "none".into()
    } else {
        active.join("|")
    }
}

fn print_usage(prog: &str) {
    eprintln!("Usage: {prog} <demo.dem> [--all] [--csv] [--json] [--summary] [--html [FILE]]");
    eprintln!();
    eprintln!("  --all      Print every packet (not just usercmds)");
    eprintln!("  --csv      Output usercmds as CSV");
    eprintln!("  --json     Output usercmds as JSON array");
    eprintln!("  --summary  Print header info and packet counts only");
    eprintln!("  --html     Generate interactive 3D HTML visualization (always includes multi-player tracks)");
    eprintln!("  --jump-threshold N  Path-break distance in units (default: auto-derived from data)");
    eprintln!();
    eprintln!("Supports: TF2, CS:S, HL2, Portal, DOD, HL2DM, GMod (demo_protocol 2/3/4)");
}

// ── HTML generation structures ────────────────────────────────────────────────

struct SampledCmd {
    tick: i32,
    pitch: f32,
    yaw: f32,
    fwd: f32,
    side: f32,
    btns: u32,
    weapon: u32,
}

enum EventValue {
    Str(String),
    Float(f32),
    Int(i32),
    Bool(bool),
    Null,
}

struct EventField {
    name: String,
    value: EventValue,
}

struct GameEvent {
    event: String,
    tick: i32,
    fields: Vec<EventField>,
}

struct EventSchema {
    name: String,
    fields: Vec<(String, u8)>, // (name, type: 1=str,2=float,3=long,4=short,5=byte,6=bool,7=local)
}

// ── Game event parsing ────────────────────────────────────────────────────────

fn scan_for_game_event_list(payload: &[u8]) -> Option<HashMap<u16, EventSchema>> {
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

fn extract_events_from_payload(
    payload: &[u8],
    tick: i32,
    schemas: &HashMap<u16, EventSchema>,
    display_events: &HashSet<&str>,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut br = BitReader::new(payload);
    let total_bits = payload.len() * 8;

    while br.bit_pos + 6 <= total_bits {
        let msg_type = match br.read_bits(6) {
            Some(v) => v,
            None => break,
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

            11 => {
                // svc_SetPause: paused(1)
                if !br.skip(1) { break; }
            }

            15 => {
                // svc_VoiceData: client(8) + proximity(8) + length_bits(16) + data[length]
                if !br.skip(16) { break; }
                let length = match br.read_bits(16) { Some(v) => v as usize, None => break };
                if !br.skip(length as u32) { break; }
            }

            17 => {
                // svc_Sounds: complex per-sound delta encoding; stop packet scan
                break;
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
                // svc_UserMessage: msg_type(8) + length_bits(11) + data[length]
                let msg_type = match br.read_bits(8) { Some(v) => v, None => break };
                let length   = match br.read_bits(11) { Some(v) => v as usize, None => break };
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

// ── BSP parsing ───────────────────────────────────────────────────────────────

// Decompress a VBSP lump that may be LZMA-compressed.
// TF2/OrangBox BSP lumps start with b"LZMA" when compressed.
// Format: magic(4) + actual_size(u32) + lzma_size(u32) + properties(5) + data
fn decompress_lzma_lump(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 4 { return None; }
    if &data[0..4] != b"LZMA" {
        return Some(data.to_vec());
    }
    if data.len() < 17 { return None; }
    let actual_size = u32::from_le_bytes(data[4..8].try_into().ok()?) as u64;
    let props = &data[12..17];  // 5 bytes: [lc/lp/pb, dict_size LE u32]
    let body = &data[17..];

    // Build LZMA "alone" format: props(5) + uncompressed_size(8 LE) + body
    let mut stream: Vec<u8> = Vec::with_capacity(13 + body.len());
    stream.extend_from_slice(props);
    stream.extend_from_slice(&actual_size.to_le_bytes());
    stream.extend_from_slice(body);

    let mut out = Vec::with_capacity(actual_size as usize);
    lzma_decompress(&mut stream.as_slice(), &mut out).ok()?;
    Some(out)
}

fn find_bsp_file(dem_path: &Path, map_name: &str) -> Option<PathBuf> {
    let map_lower = map_name.to_lowercase();

    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Some(parent) = dem_path.parent() {
            v.push(parent.join(format!("{}.bsp", map_name)));
            v.push(parent.join(format!("{}.bsp", map_lower)));
        }
        // executable dir
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                v.push(exe_dir.join(format!("{}.bsp", map_name)));
                v.push(exe_dir.join(format!("{}.bsp", map_lower)));
            }
        }
        v
    };

    for c in candidates {
        if c.exists() {
            return Some(c);
        }
    }
    None
}

// Path wrapper - opens the file and delegates to the byte-slice core. Keeps
// the existing CLI flow working unchanged; WASM goes straight through the
// `_from_bytes` variant since there's no filesystem in the browser. The
// wrapper itself is unused now that generate_html reads bytes up front and
// hands them in, but it stays as a convenience for any future direct callers.
#[allow(dead_code)]
fn extract_bsp(bsp_path: &Path) -> Option<(String, String, usize, usize, [f32; 3])> {
    let mut f = File::open(bsp_path).ok()?;
    let mut data = Vec::new();
    f.read_to_end(&mut data).ok()?;
    extract_bsp_from_bytes(&data)
}

fn extract_bsp_from_bytes(data: &[u8]) -> Option<(String, String, usize, usize, [f32; 3])> {
    if data.len() < 1036 { return None; }
    if &data[0..4] != b"VBSP" { return None; }

    // Read lump table: offset 8, 64 lumps × 16 bytes each
    let lump_raw = |i: usize| -> (usize, usize) {
        let o = 8 + i * 16;
        (le_i32(&data, o) as usize, le_i32(&data, o + 4) as usize)
    };

    // Decompress (or copy) a lump into an owned Vec
    let get_lump = |i: usize| -> Option<Vec<u8>> {
        let (off, len) = lump_raw(i);
        if off + len > data.len() { return None; }
        decompress_lzma_lump(&data[off..off + len])
    };

    let en_data = get_lump(0)?;   // entities
    let v_data  = get_lump(3)?;   // vertices
    let ti_data = get_lump(6)?;   // texinfo
    let f_data  = get_lump(7)?;   // faces
    let e_data  = get_lump(12)?;  // edges
    let se_data = get_lump(13)?;  // surfedges
    let m_data  = get_lump(14);   // models
    let di_data = get_lump(26).unwrap_or_default(); // dispinfo
    let dv_data = get_lump(33).unwrap_or_default(); // disp_verts

    let n_verts = v_data.len() / 12;
    let n_tinfo = ti_data.len() / 72;
    let n_faces = f_data.len() / 56;
    let n_edges = e_data.len() / 4;
    let n_se    = se_data.len() / 4;

    if n_verts == 0 || n_faces == 0 { return None; }

    // Parse texinfo flags (offset 64 in each 72-byte struct)
    let mut ti_flags: Vec<i32> = vec![0i32; n_tinfo];
    for i in 0..n_tinfo {
        let off = i * 72 + 64;
        if off + 4 <= ti_data.len() { ti_flags[i] = le_i32(&ti_data, off); }
    }

    // Edges: pair of u16 vertex indices
    let mut edges: Vec<(u16, u16)> = Vec::with_capacity(n_edges);
    for i in 0..n_edges {
        let o = i * 4;
        if o + 4 <= e_data.len() { edges.push((le_u16(&e_data, o), le_u16(&e_data, o + 2))); }
        else { edges.push((0, 0)); }
    }

    // Surfedges: i32 (sign encodes edge direction)
    let mut se: Vec<i32> = Vec::with_capacity(n_se);
    for i in 0..n_se {
        let o = i * 4;
        if o + 4 <= se_data.len() { se.push(le_i32(&se_data, o)); }
        else { se.push(0); }
    }

    // Vertices: float32 x3
    let mut verts_xyz: Vec<[f32; 3]> = Vec::with_capacity(n_verts);
    for i in 0..n_verts {
        let o = i * 12;
        if o + 12 <= v_data.len() {
            verts_xyz.push([le_f32(&v_data, o), le_f32(&v_data, o + 4), le_f32(&v_data, o + 8)]);
        } else {
            verts_xyz.push([0.0; 3]);
        }
    }

    // Surface flags to skip: sky2d(0x02), sky(0x04), trigger(0x40), nodraw(0x80), hint(0x100), skip(0x200)
    let skip_flags: i32 = 0x02 | 0x04 | 0x40 | 0x80 | 0x100 | 0x200;

    // Model 0 = worldspawn (static geometry + func_detail compiled in).
    // Models 1+ are brush entities - skip to avoid trigger boxes and floating origin brushes.
    // dmodel_t: mins(12) + maxs(12) + origin(12) + headnode(4) + firstface(4) + numfaces(4) = 48 bytes
    let (world_first, world_end) = match &m_data {
        Some(m) if m.len() >= 48 => {
            let ff = le_i32(m, 40) as usize;
            let nf = le_i32(m, 44) as usize;
            (ff, (ff + nf).min(n_faces))
        }
        _ => (0, n_faces),
    };

    // Collect triangles. Non-displacement faces use fan triangulation of
    // surfedge corners. Displacement faces are tessellated into a (2^power+1)²
    // grid via the algorithm from qbyte's SourceImporter
    // (~/Downloads/__init__.py): the dispinfo gives 4 face corners + a starting
    // corner; we bilinear-interp positions and offset each grid vert by its
    // DISPVERT direction × distance.
    //
    // DISPINFO (176 bytes per entry on Source v20):
    //    0..12  : startPosition (Vector)
    //   12..16  : DispVertStart (i32)
    //   16..20  : DispTriStart (i32)
    //   20..24  : power (i32)
    //  rest     : minTess / smoothingAngle / neighbors etc - ignored here.
    //
    // DISPVERT (20 bytes per entry):
    //    0..12  : vec (Vector - unit direction)
    //   12..16  : dist (f32)
    //   16..20  : alpha (f32, unused here)
    const DISPINFO_SIZE: usize = 176;
    const DISPVERT_SIZE: usize = 20;
    let n_disp = di_data.len() / DISPINFO_SIZE;

    // Decoded displacements (separate vertex pool - appended after compaction).
    let mut disp_verts_xyz: Vec<[f32; 3]> = Vec::new();
    let mut disp_tris: Vec<(u32, u32, u32)> = Vec::new();

    const MAX_TRIS: usize = 600_000;
    let mut tris: Vec<(u32, u32, u32)> = Vec::new();
    for fi in world_first..world_end {
        if tris.len() + disp_tris.len() >= MAX_TRIS { break; }
        let b = fi * 56;
        if b + 56 > f_data.len() { continue; }
        let firstedge = le_i32(&f_data, b + 4) as i32;
        let numedges  = le_i16_bytes(&f_data, b + 8) as i32;
        let ti_idx    = le_i16_bytes(&f_data, b + 10) as i32;
        let dispinfo  = le_i16_bytes(&f_data, b + 12) as i32;

        if ti_idx < 0 || ti_idx as usize >= n_tinfo { continue; }
        if ti_flags[ti_idx as usize] & skip_flags != 0 { continue; }
        if numedges < 3 { continue; }

        // Resolve corner verts via surfedges.
        let mut fv: Vec<u32> = Vec::with_capacity(numedges as usize);
        'edge: for i in 0..numedges {
            let se_idx = (firstedge + i) as usize;
            if se_idx >= se.len() { break; }
            let s = se[se_idx];
            let vi = if s >= 0 {
                let idx = s as usize;
                if idx < edges.len() { edges[idx].0 as u32 } else { continue 'edge; }
            } else {
                let idx = (-s) as usize;
                if idx < edges.len() { edges[idx].1 as u32 } else { continue 'edge; }
            };
            fv.push(vi);
        }

        if dispinfo == -1 {
            for i in 1..fv.len().saturating_sub(1) {
                tris.push((fv[0], fv[i], fv[i + 1]));
            }
            continue;
        }

        // ── Displacement face ──
        if numedges != 4 { continue; } // Source displacements are always quads.
        let di = dispinfo as usize;
        if di >= n_disp { continue; }
        let di_off = di * DISPINFO_SIZE;
        let sx = le_f32(&di_data, di_off);
        let sy = le_f32(&di_data, di_off + 4);
        let sz = le_f32(&di_data, di_off + 8);
        let dv_start = le_i32(&di_data, di_off + 12) as usize;
        let power    = le_i32(&di_data, di_off + 20);
        if !(1..=4).contains(&power) { continue; }
        let n = (1usize << power) + 1; // verts per side
        let total_dv = n * n;
        if dv_start + total_dv > dv_data.len() / DISPVERT_SIZE { continue; }

        // Look up the 4 corner positions (in surfedge order).
        let c: Vec<[f32; 3]> = fv.iter().map(|&i| {
            if (i as usize) < verts_xyz.len() { verts_xyz[i as usize] } else { [0.0; 3] }
        }).collect();

        // Rotate corners so c[start_idx] is closest to startPosition.
        let d2 = |a: &[f32; 3], b: (f32, f32, f32)| {
            let dx = a[0] - b.0; let dy = a[1] - b.1; let dz = a[2] - b.2;
            dx*dx + dy*dy + dz*dz
        };
        let start = (sx, sy, sz);
        let mut start_idx = 0usize;
        let mut best = d2(&c[0], start);
        for i in 1..4 {
            let d = d2(&c[i], start);
            if d < best { best = d; start_idx = i; }
        }
        // Pick rotation per qbyte's mapping; the 4 corners then act as
        // v00, v10, v01, v11 (bilinear basis with start at v00).
        let idxs: [usize; 4] = match start_idx {
            0 => [0, 1, 3, 2],
            1 => [1, 2, 0, 3],
            2 => [2, 3, 1, 0],
            _ => [3, 0, 2, 1],
        };
        let v00 = c[idxs[0]];
        let v10 = c[idxs[1]];
        let v01 = c[idxs[2]];
        let v11 = c[idxs[3]];

        // Allocate grid: index (y, x) → flat index y + x*n (qbyte's layout).
        let base = disp_verts_xyz.len() as u32;
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| -> [f32; 3] {
            [a[0]*(1.0-t) + b[0]*t, a[1]*(1.0-t) + b[1]*t, a[2]*(1.0-t) + b[2]*t]
        };
        let denom = (n - 1) as f32;
        for y0 in 0..n {
            let ty = y0 as f32 / denom;
            let a = lerp(v00, v01, ty);
            let b = lerp(v10, v11, ty);
            for x0 in 0..n {
                let tx = x0 as f32 / denom;
                let p = lerp(a, b, tx);
                let dv_idx = (dv_start + y0 + x0 * n) * DISPVERT_SIZE;
                let dvx = le_f32(&dv_data, dv_idx);
                let dvy = le_f32(&dv_data, dv_idx + 4);
                let dvz = le_f32(&dv_data, dv_idx + 8);
                let dist = le_f32(&dv_data, dv_idx + 12);
                disp_verts_xyz.push([
                    p[0] + dvx * dist,
                    p[1] + dvy * dist,
                    p[2] + dvz * dist,
                ]);
            }
        }
        // Triangulate the grid. Two tris per quad in alternating pattern -
        // matches Source's runtime tessellation (and visually equivalent to
        // qbyte's 8-tri fan for wireframe rendering, at 1/4 the tri budget).
        let nu = n as u32;
        let idx_of = |x: u32, y: u32| -> u32 { base + y + x * nu };
        for y0 in 0..(nu - 1) {
            for x0 in 0..(nu - 1) {
                let i00 = idx_of(x0, y0);
                let i10 = idx_of(x0 + 1, y0);
                let i01 = idx_of(x0, y0 + 1);
                let i11 = idx_of(x0 + 1, y0 + 1);
                if (x0 + y0) & 1 == 0 {
                    disp_tris.push((i00, i10, i11));
                    disp_tris.push((i00, i11, i01));
                } else {
                    disp_tris.push((i00, i10, i01));
                    disp_tris.push((i10, i11, i01));
                }
            }
        }
    }

    if tris.is_empty() && disp_tris.is_empty() { return None; }

    // Compact non-displacement geometry: remap to only used vertex indices.
    let used: Vec<u32> = {
        let mut set: HashSet<u32> = HashSet::new();
        for &(a, b, c) in &tris { set.insert(a); set.insert(b); set.insert(c); }
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        v
    };
    let mut remap: HashMap<u32, u32> = HashMap::with_capacity(used.len());
    for (ni, &oi) in used.iter().enumerate() { remap.insert(oi, ni as u32); }

    let mut compact_v: Vec<[f32; 3]> = used.iter()
        .map(|&i| if (i as usize) < verts_xyz.len() { verts_xyz[i as usize] } else { [0.0; 3] })
        .collect();
    let mut compact_t: Vec<(u32, u32, u32)> = tris.iter()
        .filter_map(|&(a, b, c)| Some((*remap.get(&a)?, *remap.get(&b)?, *remap.get(&c)?)))
        .collect();

    // Append displacement verts + tris. Displacement indices are already
    // self-consistent (they referenced disp_verts_xyz directly), so we just
    // shift them by the new base after extending compact_v.
    if !disp_tris.is_empty() {
        let shift = compact_v.len() as u32;
        compact_v.extend(disp_verts_xyz.iter().copied());
        for (a, b, c) in disp_tris {
            compact_t.push((a + shift, b + shift, c + shift));
        }
    }

    // Encode to base64
    let mut v_buf = Vec::with_capacity(compact_v.len() * 12);
    for &[x, y, z] in &compact_v {
        v_buf.extend_from_slice(&x.to_le_bytes());
        v_buf.extend_from_slice(&y.to_le_bytes());
        v_buf.extend_from_slice(&z.to_le_bytes());
    }
    let mut i_buf = Vec::with_capacity(compact_t.len() * 12);
    for &(a, b, c) in &compact_t {
        i_buf.extend_from_slice(&a.to_le_bytes());
        i_buf.extend_from_slice(&b.to_le_bytes());
        i_buf.extend_from_slice(&c.to_le_bytes());
    }

    let verts_b64 = STANDARD.encode(&v_buf);
    let idx_b64   = STANDARD.encode(&i_buf);

    // Parse entity lump for spawn origin
    let en_str = String::from_utf8_lossy(&en_data);
    let spawn = find_spawn_in_entities(&en_str).unwrap_or([0.0; 3]);

    Some((verts_b64, idx_b64, compact_v.len(), compact_t.len(), spawn))
}

fn find_spawn_in_entities(entities: &str) -> Option<[f32; 3]> {
    // Parse entity blocks: { ... } and look for classname + origin keys
    let mut chars = entities.chars().peekable();

    loop {
        // Find opening brace
        while let Some(&c) = chars.peek() {
            if c == '{' {
                chars.next();
                break;
            }
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }

        // Read until closing brace
        let mut block = String::new();
        let mut depth = 1;
        for c in chars.by_ref() {
            if c == '{' { depth += 1; }
            else if c == '}' {
                depth -= 1;
                if depth == 0 { break; }
            }
            block.push(c);
        }

        // Parse key-value pairs from block
        let mut classname = String::new();
        let mut origin = String::new();

        let mut i = 0;
        let b = block.as_bytes();
        while i < b.len() {
            // Skip whitespace
            while i < b.len() && (b[i] == b' ' || b[i] == b'\n' || b[i] == b'\r' || b[i] == b'\t') {
                i += 1;
            }
            if i >= b.len() { break; }
            // Read quoted key
            if b[i] != b'"' { i += 1; continue; }
            i += 1;
            let key_start = i;
            while i < b.len() && b[i] != b'"' { i += 1; }
            let key = std::str::from_utf8(&b[key_start..i]).unwrap_or("").trim().to_string();
            if i < b.len() { i += 1; } // skip closing quote

            // Skip whitespace
            while i < b.len() && (b[i] == b' ' || b[i] == b'\n' || b[i] == b'\r' || b[i] == b'\t') {
                i += 1;
            }
            // Read quoted value
            if i >= b.len() || b[i] != b'"' { continue; }
            i += 1;
            let val_start = i;
            while i < b.len() && b[i] != b'"' { i += 1; }
            let val = std::str::from_utf8(&b[val_start..i]).unwrap_or("").to_string();
            if i < b.len() { i += 1; }

            match key.as_str() {
                "classname" => classname = val,
                "origin" => origin = val,
                _ => {}
            }
        }

        let cls_lower = classname.to_lowercase();
        if cls_lower.contains("teamspawn") || classname == "info_player_start" {
            let parts: Vec<&str> = origin.split_whitespace().collect();
            if parts.len() == 3 {
                if let (Ok(x), Ok(y), Ok(z)) = (
                    parts[0].parse::<f32>(),
                    parts[1].parse::<f32>(),
                    parts[2].parse::<f32>(),
                ) {
                    return Some([x, y, z]);
                }
            }
        }
    }
    None
}

// ── svc_SetView extraction ────────────────────────────────────────────────────
// Parses each game-packet payload looking for svc_SetView (net message type 17
// in TF2's demo protocol, immediately after net_Tick type 3). Returns
// (tick, entity_index) pairs for every packet where svc_SetView was found.
fn extract_svc_setview(data: &[u8], game_packet_ticks: &[(i32, usize, usize)]) -> Vec<(i32, u16)> {
    let mut results = Vec::new();
    for &(tick, payload_start, payload_end) in game_packet_ticks {
        if payload_end <= payload_start { continue; }
        let payload = &data[payload_start..payload_end];
        let total_bits = payload.len() * 8;

        let read_bits = |pos: usize, n: usize| -> Option<u32> {
            if pos + n > total_bits { return None; }
            let mut v = 0u32;
            for i in 0..n {
                let bi = (pos + i) >> 3;
                v |= (((payload[bi] >> ((pos + i) & 7)) & 1) as u32) << i;
            }
            Some(v)
        };

        let mut bit_pos = 0usize;
        for _ in 0..15 {
            if bit_pos + 6 > total_bits { break; }
            let msg_type = match read_bits(bit_pos, 6) { Some(t) => t as u8, None => break };
            bit_pos += 6;
            match msg_type {
                0 => {}                  // net_NOP - no data
                3 => { bit_pos += 64; } // net_Tick - skip 64 bits
                17 => {                  // svc_SetView - 12-bit entity index
                    if let Some(entity) = read_bits(bit_pos, 12) {
                        results.push((tick, entity as u16));
                    }
                    break;
                }
                26 => break, // svc_PacketEntities - no SetView before entity updates
                _ => break,  // unknown message, stop
            }
        }
    }
    results
}

// Identify spectator-switch intervals: tick ranges [start, end) where the
// spectator was watching a non-primary entity. The primary entity is the one
// seen in the most svc_SetView events.
fn spectator_switch_intervals(setview_events: &[(i32, u16)]) -> Vec<(i32, i32)> {
    if setview_events.len() < 5 { return vec![]; }
    let mut counts = std::collections::HashMap::<u16, usize>::new();
    for &(_, e) in setview_events { *counts.entry(e).or_insert(0) += 1; }
    let primary = counts.into_iter().max_by_key(|&(_, c)| c).map(|(e, _)| e).unwrap_or(0);

    let mut intervals: Vec<(i32, i32)> = Vec::new();
    let mut switch_start: Option<i32> = None;
    let mut cur_entity: Option<u16> = None;

    for &(tick, entity) in setview_events {
        if cur_entity == Some(entity) { continue; }
        if let (Some(start), Some(prev)) = (switch_start, cur_entity) {
            if prev != primary { intervals.push((start, tick)); }
        }
        switch_start = Some(tick);
        cur_entity = Some(entity);
    }
    if let (Some(start), Some(e)) = (switch_start, cur_entity) {
        if e != primary {
            let end = setview_events.last().map(|&(t, _)| t + 1).unwrap_or(start + 1);
            intervals.push((start, end));
        }
    }
    intervals
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

fn compute_life_breaks(
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

// ── JSON serialization helpers ────────────────────────────────────────────────

fn json_f32(v: f32) -> String {
    // Format to 3 decimal places; strip trailing zeros
    let s = format!("{:.3}", v);
    // strip trailing zeros after decimal point
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    if s.is_empty() || s == "-" { "0".to_string() } else { s.to_string() }
}

fn cmds_to_json(cmds: &[SampledCmd]) -> String {
    let items: Vec<String> = cmds.iter().map(|c| {
        format!("[{},{},{},{},{},{},{}]",
            c.tick,
            json_f32(c.pitch),
            json_f32(c.yaw),
            json_f32(c.fwd),
            json_f32(c.side),
            c.btns,
            c.weapon)
    }).collect();
    format!("[{}]", items.join(","))
}

fn escape_json_str(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn events_to_json(events: &[GameEvent]) -> String {
    let items: Vec<String> = events.iter().map(|ev| {
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
                EventValue::Bool(b) => if *b { "true".to_string() } else { "false".to_string() },
                EventValue::Null => "null".to_string(),
            };
            fields.push_str(&format!(",\"{}\":{}", escape_json_str(&f.name), val_str));
        }
        fields.push('}');
        fields
    }).collect();
    format!("[{}]", items.join(","))
}

fn breaks_to_json(breaks: &[usize]) -> String {
    format!("[{}]", breaks.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","))
}

fn world_positions_to_json(positions: &[(i32, f32, f32, f32)]) -> String {
    let items: Vec<String> = positions.iter().map(|(t, x, y, z)| {
        format!("[{},{},{},{}]", t, json_f32(*x), json_f32(*y), json_f32(*z))
    }).collect();
    format!("[{}]", items.join(","))
}

fn meta_to_json(
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

fn spawn_to_json(spawn: [f32; 3]) -> String {
    format!("[{},{},{}]", json_f32(spawn[0]), json_f32(spawn[1]), json_f32(spawn[2]))
}

// ── Demo packet iterator ──────────────────────────────────────────────────────

struct DemoPacketInfo {
    cmd: u8,
    tick: i32,
    payload_start: usize,
    payload_end: usize,
}

fn iterate_demo_packets(data: &[u8], demo_protocol: i32) -> Vec<DemoPacketInfo> {
    // demo_protocol > 3 (L4D, Portal 2, CS:GO, …) adds a player_slot byte after cmd+tick
    let extra: usize = if demo_protocol > 3 { 1 } else { 0 };
    let pkt_hdr = 5 + extra;
    // Proto-4's democmdinfo is an array of Split_t[MAX_SPLITSCREEN_CLIENTS].
    // L4D1/L4D2 ship with 4 slots, Portal 2/Stanley/CS:GO with 2. Detect by
    // trying N = 4, 2, 1 on the first SIGNON/PACKET and picking the one
    // whose length-field reads as a sensible payload size.
    let mut splitscreen: usize = 1;
    if demo_protocol > 3 && data.len() > HEADER_SIZE + pkt_hdr + 100 {
        let pkt_start = HEADER_SIZE + pkt_hdr;
        for n in [4, 2, 1] {
            let len_off = pkt_start + 76 * n + 8;
            if len_off + 4 > data.len() { continue; }
            let length = le_i32(data, len_off);
            let payload_end = len_off + 4 + length as usize;
            if length > 0 && (length as usize) < (data.len() - pkt_start) && payload_end < data.len() {
                splitscreen = n;
                break;
            }
        }
    }
    let democmdinfo = 76 * splitscreen;
    let preamble = democmdinfo + 12;

    let mut packets = Vec::new();
    let mut offset = HEADER_SIZE;

    while offset < data.len() {
        if offset + 5 > data.len() { break; }
        let cmd = data[offset];
        let tick = le_i32(data, offset + 1);
        offset += pkt_hdr;

        match cmd {
            3 => {
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
            }
            7 => {
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
                break;
            }
            1 | 2 => {
                if offset + preamble > data.len() { break; }
                let length = le_i32(data, offset + democmdinfo + 8) as usize;
                let payload_start = offset + preamble;
                let payload_end = payload_start + length;
                if payload_end > data.len() { break; }
                packets.push(DemoPacketInfo { cmd, tick, payload_start, payload_end });
                offset = payload_end;
            }
            4 => {
                // ConsoleCmd
                if offset + 4 > data.len() { break; }
                let length = le_i32(data, offset) as usize;
                offset += 4 + length;
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
            }
            5 => {
                // UserCmd
                if offset + 8 > data.len() { break; }
                let length = le_i32(data, offset + 4) as usize;
                let next = offset + 8 + length;
                if next > data.len() { break; }
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
                offset = next;
            }
            6 | 8 => {
                // DataTables / StringTables
                if offset + 4 > data.len() { break; }
                let length = le_i32(data, offset) as usize;
                let next = (offset + 4 + length).min(data.len());
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
                offset = next;
            }
            _ => break,
        }
    }

    packets
}

// ── HTML generation ───────────────────────────────────────────────────────────

const HTML_TEMPLATE: &str = include_str!("template.html");

const MAX_CMDS_EMBED: usize = 20_000;

// ── Player info from DEM_STRINGTABLES ────────────────────────────────────────

fn parse_userinfo_from_demo(data: &[u8], proto: i32) -> (HashMap<i32, (String, bool)>, HashMap<usize, i32>) {
    let extra: usize = if proto > 3 { 1 } else { 0 };
    let pkt_hdr = 5 + extra;
    let mut offset = HEADER_SIZE;
    let mut result = HashMap::new();
    let mut slot_to_uid: HashMap<usize, i32> = HashMap::new();

    while offset < data.len() {
        if offset + 5 > data.len() { break; }
        let cmd = data[offset];
        offset += pkt_hdr;

        match cmd {
            7 => break,
            1 | 2 => {
                if offset + 88 > data.len() { break; }
                let length = le_i32(data, offset + 84) as usize;
                offset = offset.saturating_add(88 + length);
            }
            3 => {}
            4 => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(data, offset) as usize;
                offset = offset.saturating_add(4 + length);
            }
            5 => {
                if offset + 8 > data.len() { break; }
                let length = le_i32(data, offset + 4) as usize;
                offset = offset.saturating_add(8 + length);
            }
            6 | 8 => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(data, offset) as usize;
                let payload_start = offset + 4;
                let payload_end = (payload_start + length).min(data.len());
                if cmd == 8 && payload_end > payload_start {
                    if let Some((m, s)) = try_parse_userinfo_tables(&data[payload_start..payload_end]) {
                        result.extend(m);
                        slot_to_uid.extend(s);
                    }
                }
                offset = payload_end;
            }
            _ => break,
        }
    }
    (result, slot_to_uid)
}

// (name, is_fake)  - is_fake = player_info_t.fakeplayer (offset 124)
fn try_parse_userinfo_tables(payload: &[u8]) -> Option<(HashMap<i32, (String, bool)>, HashMap<usize, i32>)> {
    let mut br = BitReader::new(payload);
    let num_tables = br.read_bits(8)? as usize;
    if num_tables == 0 || num_tables > 64 { return None; }
    let mut result = HashMap::new();
    let mut slot_to_uid: HashMap<usize, i32> = HashMap::new();

    for _t in 0..num_tables {
        let table_name = br.read_cstring_any(64)?;
        let num_strings = br.read_bits(16)? as usize;
        if num_strings > 4096 { return None; }
        let is_userinfo = table_name == "userinfo";

        for slot in 0..num_strings {
            let player_name = br.read_cstring_any(512)?;
            let has_ud = br.read_bits(1)? != 0;
            let mut uid: Option<i32> = None;
            let mut is_fake = false;
            if has_ud {
                let ud_len = br.read_bits(16)? as usize;
                if ud_len > 8192 { return None; }
                if is_userinfo && ud_len >= 52 {
                    let mut bytes = vec![0u8; ud_len];
                    for i in 0..ud_len { bytes[i] = br.read_bits(8)? as u8; }
                    // player_info_t: version(8)+xuid(8)+name(32)=48, userID(4) at 48
                    uid = Some(i32::from_le_bytes([bytes[48], bytes[49], bytes[50], bytes[51]]));
                    // fakeplayer at offset 124 (after guid[33]+pad+friendsID+friendsName)
                    if ud_len > 124 { is_fake = bytes[124] != 0; }
                    // actual player name is at bytes[16..48] (after version+xuid), null-terminated
                    let name_end = bytes[16..48.min(ud_len)].iter().position(|&b| b == 0).unwrap_or(32);
                    let actual_name = String::from_utf8_lossy(&bytes[16..16 + name_end]).into_owned();
                    if !actual_name.is_empty() {
                        let id = uid.unwrap_or(slot as i32 + 1);
                        if id > 0 {
                            result.insert(id, (actual_name, is_fake));
                            slot_to_uid.insert(slot, id);
                        }
                    }
                    continue;
                } else {
                    for _ in 0..ud_len { br.read_bits(8)?; }
                }
            }
            if is_userinfo && !player_name.is_empty() {
                let id = uid.unwrap_or(slot as i32 + 1);
                if id > 0 {
                    result.insert(id, (player_name, is_fake));
                    slot_to_uid.insert(slot, id);
                }
            }
        }

        let has_cs = br.read_bits(1)? != 0;
        if has_cs {
            let n = br.read_bits(16)? as usize;
            for _ in 0..n {
                br.read_cstring_any(512)?;
                let hud = br.read_bits(1)? != 0;
                if hud {
                    let l = br.read_bits(16)? as usize;
                    for _ in 0..l { br.read_bits(8)?; }
                }
            }
        }
    }
    if result.is_empty() { None } else { Some((result, slot_to_uid)) }
}

fn display_events_for_game(game_dir: &str) -> HashSet<&'static str> {
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

fn multi_tracks_to_json(data: &multi_player::MultiPlayerData) -> String {
    // Per-entity tracks → {"3":[[tick,x,y,z], ...], "4":[...], ...}
    // Subsampled to ~1500 points/entity. We include any entity that has
    // ANY track samples OR appears in life_states (so e.g. the spectator
    // entity shows up in the sidebar even with zero position updates).
    const TARGET_POINTS: usize = 1500;
    let mut eids: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    eids.extend(data.tracks.keys().copied());
    eids.extend(data.life_states.keys().copied());
    eids.extend(data.names.keys().copied());

    let mut entries: Vec<String> = eids.into_iter().map(|eid| {
        let empty = Vec::new();
        let samples = data.tracks.get(&eid).unwrap_or(&empty);
        let reduced = multi_player::subsample(samples, TARGET_POINTS);
        let pts: Vec<String> = reduced.iter()
            .map(|(t, x, y, z)| format!("[{},{},{},{}]", t, json_f32(*x), json_f32(*y), json_f32(*z)))
            .collect();
        format!("\"{}\":[{}]", eid, pts.join(","))
    }).collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

fn multi_life_states_to_json(data: &multi_player::MultiPlayerData) -> String {
    let mut entries: Vec<String> = data.life_states.iter().map(|(eid, states)| {
        let pts: Vec<String> = states.iter().map(|(t, s)| format!("[{},{}]", t, s)).collect();
        format!("\"{}\":[{}]", eid, pts.join(","))
    }).collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

fn multi_weapons_to_json(data: &multi_player::MultiPlayerData) -> String {
    // Per-player active-weapon stream: entity_id → [[tick, weapon_eid], ...].
    let mut entries: Vec<String> = data.weapons.iter().map(|(eid, w)| {
        let pts: Vec<String> = w.iter().map(|(t, wid)| format!("[{},{}]", t, wid)).collect();
        format!("\"{}\":[{}]", eid, pts.join(","))
    }).collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

fn multi_weapon_classes_to_json(data: &multi_player::MultiPlayerData) -> String {
    // weapon_eid → class name; strip the "CTF" prefix and lowercase to look
    // tidy in the UI (`CTFRocketLauncher` → `rocketlauncher`).
    let mut entries: Vec<String> = data.weapon_classes.iter().map(|(eid, name)| {
        let trimmed = name.strip_prefix("CTFWeapon").or_else(|| name.strip_prefix("CTF")).unwrap_or(name);
        format!("\"{}\":\"{}\"", eid, escape_json_str(&trimmed.to_lowercase()))
    }).collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

fn multi_yaws_to_json(data: &multi_player::MultiPlayerData) -> String {
    // Per-entity eye-angle yaw stream subsampled to a manageable size. Used by
    // the input panel to project velocity into the player's local frame for
    // proper WSAD reconstruction. Drop deg-fractions to 1 decimal to shave
    // bytes since the panel rounds to 90° quadrants anyway.
    const TARGET: usize = 1500;
    let mut entries: Vec<String> = data.yaws.iter().map(|(eid, ys)| {
        let stride = if ys.len() > TARGET { (ys.len() + TARGET - 1) / TARGET } else { 1 };
        // [tick, yaw, pitch] - pitch (3rd element) drives the first-person
        // camera on proto-4 demos; older JS consumers read only [0]/[1].
        let mut pts: Vec<String> = ys.iter().step_by(stride)
            .map(|(t, y, p)| format!("[{},{:.1},{:.1}]", t, y, p)).collect();
        if stride > 1 {
            if let Some(last) = ys.last() {
                pts.push(format!("[{},{:.1},{:.1}]", last.0, last.1, last.2));
            }
        }
        format!("\"{}\":[{}]", eid, pts.join(","))
    }).collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

fn multi_names_to_json(data: &multi_player::MultiPlayerData) -> String {
    let mut entries: Vec<String> = data.names.iter().map(|(eid, meta)| {
        let aliases: Vec<String> = meta.aliases.iter()
            .map(|a| format!("\"{}\"", escape_json_str(a))).collect();
        format!(
            "\"{}\":{{\"name\":\"{}\",\"steam_id\":\"{}\",\"user_id\":{},\"is_fake\":{},\"is_hltv\":{},\"aliases\":[{}]}}",
            eid,
            escape_json_str(&meta.name),
            escape_json_str(&meta.steam_id),
            meta.user_id,
            meta.is_fake,
            meta.is_hltv,
            aliases.join(","),
        )
    }).collect();
    entries.sort();
    format!("{{{}}}", entries.join(","))
}

// CLI wrapper - reads the dem + (optional) bsp from disk, delegates to the
// pure-bytes core, then writes the result. Keeps the existing command-line
// behaviour intact while the WASM build calls the core directly.
fn generate_html(dem_path: &Path, output_path: &Path, jump_threshold: f32) -> io::Result<()> {
    eprintln!("Reading {} ...", dem_path.file_name().unwrap_or_default().to_string_lossy());
    let mut file = File::open(dem_path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
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
    let name_hint = dem_path.file_name().unwrap_or_default().to_string_lossy().into_owned();
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
    let packets = iterate_demo_packets(&data, proto);

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
        let mut offset = HEADER_SIZE;
        while offset < data.len() {
            if offset + 5 > data.len() { break; }
            let cmd = data[offset];
            let tick = le_i32(&data, offset + 1);
            match cmd {
                7 => break,
                1 | 2 => {
                    let p = offset + pkt_hdr;
                    if p + DEMOCMDINFO_SIZE + 12 > data.len() { break; }
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
                    let length = le_i32(&data, p + DEMOCMDINFO_SIZE + 8) as usize;
                    offset = p + DEMOCMDINFO_SIZE + 12 + length;
                }
                3 => { offset += pkt_hdr; }
                4 => {
                    let p = offset + pkt_hdr;
                    if p + 4 > data.len() { break; }
                    let length = le_i32(&data, p) as usize;
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
                    offset = p + 4 + length;
                }
                5 => {
                    let p = offset + pkt_hdr;
                    if p + 8 > data.len() { break; }
                    let out_seq = le_i32(&data, p);
                    let length = le_i32(&data, p + 4) as usize;
                    let next = p + 8 + length;
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
                6 | 8 => {
                    let p = offset + pkt_hdr;
                    if p + 4 > data.len() { break; }
                    let length = le_i32(&data, p) as usize;
                    offset = (p + 4 + length).min(data.len());
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
            let evs = extract_events_from_payload(payload, tick, schemas, &display_ev);
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
    let (userinfo, _slot_to_uid) = parse_userinfo_from_demo(&data, header.demo_protocol);
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
    let (multi_tracks_json, multi_names_json, multi_life_json, multi_yaws_json, multi_weps_json, multi_wep_classes_json, primary_eid_json) = {
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
                    multi_yaws_to_json(&data),
                    multi_weapons_to_json(&data),
                    multi_weapon_classes_to_json(&data),
                    primary_str,
                )
            }
            Err(e) => {
                eprintln!(" failed: {}", e);
                ("{}".to_string(), "{}".to_string(), "{}".to_string(), "{}".to_string(), "{}".to_string(), "{}".to_string(), "null".to_string())
            }
        }
    };
    html = html.replace("__ENTITY_TRACKS__", &multi_tracks_json);
    html = html.replace("__ENTITY_NAMES__", &multi_names_json);
    html = html.replace("__ENTITY_LIFE_STATES__", &multi_life_json);
    html = html.replace("__ENTITY_YAWS__", &multi_yaws_json);
    html = html.replace("__ENTITY_WEAPONS__", &multi_weps_json);
    html = html.replace("__WEAPON_CLASSES__", &multi_wep_classes_json);
    html = html.replace("__PRIMARY_ENTITY__", &primary_eid_json);
    html = html.replace("__VIEW_SWITCHES__", &view_switches_json);

    Ok(html)
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage(&args[0]);
        std::process::exit(if args.len() < 2 { 1 } else { 0 });
    }

    let filename = &args[1];

    // Check for --jump-threshold N. Default 0 = "auto" - the HTML template
    // will derive the cutoff from the 99th-percentile position delta when
    // META.jump_threshold is 0.
    let jump_threshold: f32 = {
        let mut v = 0.0_f32;
        if let Some(i) = args.iter().position(|a| a == "--jump-threshold") {
            if let Some(s) = args.get(i + 1) {
                if let Ok(n) = s.parse::<f32>() {
                    if n > 0.0 { v = n; }
                }
            }
        }
        v
    };

    // Check for --html flag
    let html_idx = args.iter().position(|a| a == "--html");
    if let Some(idx) = html_idx {
        // Determine output path - must not consume the value following --jump-threshold
        let next = args.get(idx + 1);
        let output_path = match next {
            Some(s) if !s.starts_with("--") && s != "--jump-threshold" => {
                // Make sure this token isn't the numeric arg to --jump-threshold appearing before --html
                PathBuf::from(s)
            }
            _ => Path::new(filename).with_extension("html"),
        };
        let dem_path = Path::new(filename);
        return generate_html(dem_path, &output_path, jump_threshold);
    }

    let show_all = args.iter().any(|a| a == "--all");
    let csv_mode = args.iter().any(|a| a == "--csv");
    let json_mode = args.iter().any(|a| a == "--json");
    let summary_only = args.iter().any(|a| a == "--summary");

    let mut file = File::open(filename)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    let header = parse_header(&data).unwrap_or_else(|| {
        eprintln!("Error: not a valid HL2DEMO file (bad magic)");
        std::process::exit(1);
    });

    // ── Print header ─────────────────────────────────────────────────────────
    if !csv_mode && !json_mode {
        let stem = Path::new(filename)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        println!("╔══ {stem}");
        println!("║  Map      : {}", header.map_name);
        println!("║  Client   : {}", header.client_name);
        println!("║  Server   : {}", header.server_name);
        println!("║  Game     : {}", header.game_dir);
        println!(
            "║  Protocols: demo={} net={}",
            header.demo_protocol, header.net_protocol
        );
        println!(
            "║  Length   : {:.2}s  ticks={}  frames={}  signon_len={}",
            header.playback_time, header.ticks, header.frames, header.sign_on_length
        );
        println!("╚══ ({} bytes on disk)", data.len());
        println!();
    }

    if summary_only {
        return Ok(());
    }

    if csv_mode {
        println!("tick,cmd_num,pitch,yaw,roll,fwd,side,up,buttons,impulse,weapon,mousedx,mousedy");
    } else if json_mode {
        println!("[");
    }

    // demo_protocol > 3 adds a player_slot byte after cmd+tick
    let pkt_extra: usize = if header.demo_protocol > 3 { 1 } else { 0 };
    let ph = 5 + pkt_extra; // packet header size (cmd+tick+[slot])

    // ── Packet loop ───────────────────────────────────────────────────────────
    let mut offset = HEADER_SIZE;
    let mut pkt_num = 0u32;
    let mut counts = [0u32; 9]; // indexed by cmd byte (1-8)
    let mut usercmd_count = 0u32;
    let mut json_first = true;
    let mut last_tick = 0i32;

    while offset < data.len() {
        if offset + 5 > data.len() {
            break;
        }
        let cmd = data[offset];
        let tick = le_i32(&data, offset + 1);
        if tick > last_tick {
            last_tick = tick;
        }
        if (1..=8).contains(&cmd) {
            counts[cmd as usize] += 1;
        }

        match cmd {
            // ── Stop ──────────────────────────────────────────────────────────
            DEM_STOP => {
                if !csv_mode && !json_mode {
                    println!("[{pkt_num:>6}] STOP   tick={tick}");
                }
                break;
            }

            // ── Signon / Packet ───────────────────────────────────────────────
            DEM_SIGNON | DEM_PACKET => {
                let base = offset + ph;
                if base + DEMOCMDINFO_SIZE + 12 > data.len() { break; }
                let in_seq = le_i32(&data, base + DEMOCMDINFO_SIZE);
                let out_seq = le_i32(&data, base + DEMOCMDINFO_SIZE + 4);
                let length = le_i32(&data, base + DEMOCMDINFO_SIZE + 8) as usize;
                let next = base + DEMOCMDINFO_SIZE + 12 + length;
                if next > data.len() { break; }
                if show_all && !csv_mode && !json_mode {
                    let label = if cmd == DEM_SIGNON { "SIGNON " } else { "PACKET " };
                    println!("[{pkt_num:>6}] {label} tick={tick:>7}  in={in_seq}  out={out_seq}  len={length}");
                }
                offset = next;
            }

            DEM_SYNCTICK => {
                if show_all && !csv_mode && !json_mode {
                    println!("[{pkt_num:>6}] SYNCTICK tick={tick}");
                }
                offset += ph;
            }

            DEM_CONSOLECMD => {
                let p = offset + ph;
                if p + 4 > data.len() { break; }
                let length = le_i32(&data, p) as usize;
                let next = p + 4 + length;
                if next > data.len() { break; }
                if !csv_mode && !json_mode {
                    let s = read_cstring(&data, p + 4, length);
                    println!("[{pkt_num:>6}] CONSOLE  tick={tick:>7}  \"{s}\"");
                }
                offset = next;
            }

            DEM_USERCMD => {
                let p = offset + ph;
                if p + 8 > data.len() { break; }
                let out_seq = le_i32(&data, p);
                let length = le_i32(&data, p + 4) as usize;
                let next = p + 8 + length;
                if next > data.len() { break; }
                let ucmd_bytes = &data[p + 8..next];
                offset = next;

                // Skip usercmd parsing for old net protocols (garbage output)
                if header.net_protocol <= 7 { pkt_num += 1; continue; }

                usercmd_count += 1;
                match parse_usercmd(ucmd_bytes) {
                    Some(ucmd) => {
                        let cmd_num = ucmd.command_number.unwrap_or(out_seq as u32);

                        if csv_mode {
                            println!(
                                "{tick},{cmd_num},{},{},{},{},{},{},{},{},{},{},{}",
                                ucmd.pitch.map_or(String::new(), |v| format!("{v:.4}")),
                                ucmd.yaw.map_or(String::new(), |v| format!("{v:.4}")),
                                ucmd.roll.map_or(String::new(), |v| format!("{v:.4}")),
                                ucmd.forwardmove.map_or(String::new(), |v| format!("{v:.2}")),
                                ucmd.sidemove.map_or(String::new(), |v| format!("{v:.2}")),
                                ucmd.upmove.map_or(String::new(), |v| format!("{v:.2}")),
                                ucmd.buttons.map_or(String::new(), |v| v.to_string()),
                                ucmd.impulse.map_or(String::new(), |v| v.to_string()),
                                ucmd.weaponselect.map_or(String::new(), |v| v.to_string()),
                                ucmd.mousedx.map_or(String::new(), |v| v.to_string()),
                                ucmd.mousedy.map_or(String::new(), |v| v.to_string()),
                            );
                        } else if json_mode {
                            if !json_first {
                                println!(",");
                            }
                            json_first = false;
                            print!("  {{\"tick\":{tick},\"cmd\":{cmd_num}");
                            if let Some(v) = ucmd.pitch {
                                print!(",\"pitch\":{v:.4}");
                            }
                            if let Some(v) = ucmd.yaw {
                                print!(",\"yaw\":{v:.4}");
                            }
                            if let Some(v) = ucmd.roll {
                                print!(",\"roll\":{v:.4}");
                            }
                            if let Some(v) = ucmd.forwardmove {
                                print!(",\"fwd\":{v:.2}");
                            }
                            if let Some(v) = ucmd.sidemove {
                                print!(",\"side\":{v:.2}");
                            }
                            if let Some(v) = ucmd.upmove {
                                print!(",\"up\":{v:.2}");
                            }
                            if let Some(v) = ucmd.buttons {
                                print!(
                                    ",\"buttons\":{v},\"buttons_str\":\"{}\"",
                                    fmt_buttons(v)
                                );
                            }
                            if let Some(v) = ucmd.impulse {
                                print!(",\"impulse\":{v}");
                            }
                            if let Some(v) = ucmd.weaponselect {
                                print!(",\"weapon\":{v}");
                            }
                            if let Some(v) = ucmd.weaponsubtype {
                                print!(",\"weapon_sub\":{v}");
                            }
                            if let Some(v) = ucmd.mousedx {
                                print!(",\"mousedx\":{v}");
                            }
                            if let Some(v) = ucmd.mousedy {
                                print!(",\"mousedy\":{v}");
                            }
                            print!("}}");
                        } else {
                            println!(
                                "[{pkt_num:>6}] USERCMD  tick={tick:>7}  cmd={cmd_num}  seq={out_seq}"
                            );
                            if let (Some(pitch), Some(yaw)) = (ucmd.pitch, ucmd.yaw) {
                                println!(
                                    "               view  pitch={pitch:>9.3}°  yaw={yaw:>9.3}°  roll={:.3}°",
                                    ucmd.roll.unwrap_or(0.0)
                                );
                            }
                            let fwd = ucmd.forwardmove.unwrap_or(0.0);
                            let side = ucmd.sidemove.unwrap_or(0.0);
                            let up = ucmd.upmove.unwrap_or(0.0);
                            if fwd != 0.0 || side != 0.0 || up != 0.0 {
                                println!(
                                    "               move  fwd={fwd:>8.1}  side={side:>8.1}  up={up:>8.1}"
                                );
                            }
                            if let Some(btn) = ucmd.buttons {
                                if btn != 0 {
                                    println!(
                                        "               keys  {}  (0x{btn:08x})",
                                        fmt_buttons(btn)
                                    );
                                }
                            }
                            if let Some(dx) = ucmd.mousedx {
                                println!(
                                    "               mouse dx={dx}  dy={}",
                                    ucmd.mousedy.unwrap_or(0)
                                );
                            }
                            if let Some(w) = ucmd.weaponselect {
                                println!(
                                    "               weapon slot={w}  sub={}",
                                    ucmd.weaponsubtype.unwrap_or(0)
                                );
                            }
                        }
                    }
                    None => {
                        if !csv_mode && !json_mode {
                            eprintln!(
                                "[{pkt_num:>6}] USERCMD  tick={tick:>7}  (parse failed, {length} bytes)"
                            );
                        }
                    }
                }
                // offset already set before the net_protocol guard
            }

            DEM_DATATABLES => {
                let p = offset + ph;
                if p + 4 > data.len() { break; }
                let length = le_i32(&data, p) as usize;
                let next = p + 4 + length;
                if next > data.len() { break; }
                if show_all && !csv_mode && !json_mode {
                    println!("[{pkt_num:>6}] DATATABLES tick={tick:>7}  len={length}");
                }
                offset = next;
            }

            DEM_STRINGTABLES => {
                let p = offset + ph;
                if p + 4 > data.len() { break; }
                let length = le_i32(&data, p) as usize;
                if show_all && !csv_mode && !json_mode {
                    println!("[{pkt_num:>6}] STRTABLES  tick={tick:>7}  len={length}");
                }
                offset = (p + 4 + length).min(data.len());
            }

            other => {
                if !csv_mode && !json_mode {
                    eprintln!(
                        "Unknown cmd={other} at offset=0x{offset:x}, stopping."
                    );
                }
                break;
            }
        }

        pkt_num += 1;
    }

    // ── Close JSON ────────────────────────────────────────────────────────────
    if json_mode {
        if !json_first {
            println!();
        }
        println!("]");
        return Ok(());
    }

    // ── Summary ───────────────────────────────────────────────────────────────
    if !csv_mode {
        println!();
        println!("╔══ Summary");
        println!("║  Packets parsed   : {pkt_num}");
        println!("║  Last tick seen   : {last_tick}");
        println!(
            "║  Signon           : {}",
            counts[DEM_SIGNON as usize]
        );
        println!(
            "║  Packet (game)    : {}",
            counts[DEM_PACKET as usize]
        );
        println!(
            "║  ConsoleCmd       : {}",
            counts[DEM_CONSOLECMD as usize]
        );
        println!("║  UserCmd (inputs) : {usercmd_count}");
        println!(
            "║  DataTables       : {}",
            counts[DEM_DATATABLES as usize]
        );
        println!(
            "║  StringTables     : {}",
            counts[DEM_STRINGTABLES as usize]
        );
        println!("╚══");
    }

    Ok(())
}
