use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::File;
use std::io::{self, Read, Write as IoWrite};
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};

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
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, bit_pos: 0 }
    }

    fn new_at(data: &'a [u8], pos: usize) -> Self {
        BitReader { data, bit_pos: pos }
    }

    fn read_bits(&mut self, n: u32) -> Option<u32> {
        if self.data.len() * 8 < self.bit_pos + n as usize {
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
        if self.data.len() * 8 < self.bit_pos + n as usize {
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
    eprintln!("  --html     Generate interactive 3D HTML visualization");
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
            0 => continue,

            3 => {
                // net_Tick: skip 64 bits
                if !br.skip(64) { break; }
            }

            18 => {
                // svc_SetView: entindex(11)
                if !br.skip(11) { break; }
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
                // svc_PacketEntities — skip
                if br.bit_pos + 11 > total_bits { break; }
                if br.read_bits(11).is_none() { break; } // max_entries
                let is_delta = match br.read_bits(1) {
                    Some(v) => v != 0,
                    None => break,
                };
                if is_delta {
                    if !br.skip(32) { break; } // delta_tick
                }
                if !br.skip(1) { break; } // baseline
                if !br.skip(1) { break; } // update_baseline
                if br.bit_pos + 20 > total_bits { break; }
                let length = match br.read_bits(20) {
                    Some(v) => v as usize,
                    None => break,
                };
                if !br.skip(1) { break; } // is_compressed
                if br.bit_pos + length > total_bits { break; }
                br.bit_pos += length;
            }

            30 => {
                // svc_GameEventList — skip
                if br.bit_pos + 29 > total_bits { break; }
                if br.read_bits(9).is_none() { break; }
                let total_length = match br.read_bits(20) {
                    Some(v) => v as usize,
                    None => break,
                };
                if br.bit_pos + total_length > total_bits { break; }
                br.bit_pos += total_length;
            }

            _ => break,
        }
    }

    events
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

fn extract_bsp(bsp_path: &Path) -> Option<(String, String, usize, usize, [f32; 3])> {
    let mut f = File::open(bsp_path).ok()?;
    let mut data = Vec::new();
    f.read_to_end(&mut data).ok()?;

    if data.len() < 1036 {
        return None;
    }

    // Verify magic VBSP
    if &data[0..4] != b"VBSP" {
        return None;
    }

    // Read lump table: 64 lumps x 16 bytes starting at offset 8
    // Each lump: offset(4) + length(4) + version(4) + id(4)
    let lump = |i: usize| -> (usize, usize) {
        let o = 8 + i * 16;
        let offset = le_i32(&data, o) as usize;
        let length = le_i32(&data, o + 4) as usize;
        (offset, length)
    };

    let (en_off, en_len) = lump(0);  // entities
    let (v_off, v_len) = lump(3);    // vertices
    let (ti_off, ti_len) = lump(6);  // texinfo
    let (f_off, f_len) = lump(7);    // faces
    let (e_off, e_len) = lump(12);   // edges
    let (se_off, se_len) = lump(13); // surfedges

    // Bounds check
    let check = |off: usize, len: usize| -> bool {
        off + len <= data.len()
    };
    if !check(en_off, en_len) || !check(v_off, v_len) || !check(ti_off, ti_len)
        || !check(f_off, f_len) || !check(e_off, e_len) || !check(se_off, se_len)
    {
        return None;
    }

    let n_verts = v_len / 12;
    let n_tinfo = ti_len / 72;
    let n_faces = f_len / 56;
    let n_edges = e_len / 4;
    let n_se = se_len / 4;

    // Parse texinfo flags (offset 64 within each 72-byte struct)
    let mut ti_flags: Vec<i32> = Vec::with_capacity(n_tinfo);
    for i in 0..n_tinfo {
        let off = ti_off + i * 72 + 64;
        if off + 4 <= data.len() {
            ti_flags.push(le_i32(&data, off));
        } else {
            ti_flags.push(0);
        }
    }

    // Edges: pairs of u16 vertex indices
    let mut edges: Vec<(u16, u16)> = Vec::with_capacity(n_edges);
    for i in 0..n_edges {
        let off = e_off + i * 4;
        if off + 4 <= data.len() {
            edges.push((le_u16(&data, off), le_u16(&data, off + 2)));
        } else {
            edges.push((0, 0));
        }
    }

    // Surface edges: i32 (sign = direction)
    let mut se: Vec<i32> = Vec::with_capacity(n_se);
    for i in 0..n_se {
        let off = se_off + i * 4;
        if off + 4 <= data.len() {
            se.push(le_i32(&data, off));
        } else {
            se.push(0);
        }
    }

    // Vertices: float32 x3
    let mut verts_xyz: Vec<[f32; 3]> = Vec::with_capacity(n_verts);
    for i in 0..n_verts {
        let off = v_off + i * 12;
        if off + 12 <= data.len() {
            verts_xyz.push([le_f32(&data, off), le_f32(&data, off + 4), le_f32(&data, off + 8)]);
        } else {
            verts_xyz.push([0.0, 0.0, 0.0]);
        }
    }

    // BSP skip flags: sky2=2, sky=4, nodraw=128, hint=256, skip=512
    let bsp_skip: i32 = 0x0004 | 0x0002 | 0x0080 | 0x0200 | 0x0100;

    // Collect triangles
    let mut tris: Vec<(u32, u32, u32)> = Vec::new();
    for fi in 0..n_faces {
        let b = f_off + fi * 56;
        if b + 56 > data.len() { continue; }
        // firstedge at b+4 (i32), numedges at b+8 (i16), texinfo_idx at b+10 (i16), dispinfo at b+12 (i16)
        let firstedge = le_i32(&data, b + 4) as i32;
        let numedges = le_i16_bytes(&data, b + 8) as i32;
        let ti_idx = le_i16_bytes(&data, b + 10) as i32;
        let dispinfo = le_i16_bytes(&data, b + 12) as i32;

        if dispinfo != -1 { continue; }
        if ti_idx < 0 || ti_idx as usize >= n_tinfo { continue; }
        if ti_flags[ti_idx as usize] & bsp_skip != 0 { continue; }
        if numedges < 3 { continue; }

        let mut fv: Vec<u32> = Vec::with_capacity(numedges as usize);
        for i in 0..numedges {
            let se_idx = (firstedge + i) as usize;
            if se_idx >= se.len() { break; }
            let s = se[se_idx];
            let vi = if s >= 0 {
                let idx = s as usize;
                if idx < edges.len() { edges[idx].0 as u32 } else { continue }
            } else {
                let idx = (-s) as usize;
                if idx < edges.len() { edges[idx].1 as u32 } else { continue }
            };
            fv.push(vi);
        }

        // Fan triangulate
        if fv.len() >= 3 {
            for i in 1..fv.len() - 1 {
                tris.push((fv[0], fv[i], fv[i + 1]));
            }
        }
    }

    // Compact: only used vertices
    let used: Vec<u32> = {
        let mut set: HashSet<u32> = HashSet::new();
        for &(a, b, c) in &tris {
            set.insert(a);
            set.insert(b);
            set.insert(c);
        }
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        v
    };

    let mut remap: HashMap<u32, u32> = HashMap::new();
    for (new_idx, &old_idx) in used.iter().enumerate() {
        remap.insert(old_idx, new_idx as u32);
    }

    let compact_v: Vec<[f32; 3]> = used.iter()
        .map(|&i| if (i as usize) < verts_xyz.len() { verts_xyz[i as usize] } else { [0.0; 3] })
        .collect();

    let compact_t: Vec<(u32, u32, u32)> = tris.iter()
        .filter_map(|&(a, b, c)| {
            let ra = *remap.get(&a)?;
            let rb = *remap.get(&b)?;
            let rc = *remap.get(&c)?;
            Some((ra, rb, rc))
        })
        .collect();

    // Encode vertices as float32 LE, then base64
    let mut v_buf: Vec<u8> = Vec::with_capacity(compact_v.len() * 12);
    for &[x, y, z] in &compact_v {
        v_buf.extend_from_slice(&x.to_le_bytes());
        v_buf.extend_from_slice(&y.to_le_bytes());
        v_buf.extend_from_slice(&z.to_le_bytes());
    }

    // Encode indices as u32 LE, then base64
    let mut i_buf: Vec<u8> = Vec::with_capacity(compact_t.len() * 12);
    for &(a, b, c) in &compact_t {
        i_buf.extend_from_slice(&a.to_le_bytes());
        i_buf.extend_from_slice(&b.to_le_bytes());
        i_buf.extend_from_slice(&c.to_le_bytes());
    }

    let verts_b64 = STANDARD.encode(&v_buf);
    let idx_b64 = STANDARD.encode(&i_buf);

    // Parse entities lump: find first info_player_teamspawn / info_player_start with origin
    let mut spawn = [0.0f32; 3];
    if en_off + en_len <= data.len() {
        let en_bytes = &data[en_off..en_off + en_len];
        let en_str = String::from_utf8_lossy(en_bytes);
        if let Some(sp) = find_spawn_in_entities(&en_str) {
            spawn = sp;
        }
    }

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

// ── Life/teleport break computation ──────────────────────────────────────────

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

    let mut spawn_events: Vec<i32> = events.iter()
        .filter(|e| e.event == "player_spawn")
        .map(|e| e.tick)
        .collect();
    spawn_events.sort_unstable();

    let mut death_events: Vec<i32> = events.iter()
        .filter(|e| e.event == "player_death")
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
        format!("[{},{},{},{},{},{}]",
            c.tick,
            json_f32(c.pitch),
            json_f32(c.yaw),
            json_f32(c.fwd),
            json_f32(c.side),
            c.btns)
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

fn meta_to_json(map: &str, client: &str, duration: f32, ncmds: usize) -> String {
    format!(
        "{{\"map\":\"{}\",\"client\":\"{}\",\"duration\":{:.2},\"ncmds\":{}}}",
        escape_json_str(map),
        escape_json_str(client),
        duration,
        ncmds
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
    let mut packets = Vec::new();
    let mut offset = HEADER_SIZE;

    while offset < data.len() {
        if offset + 5 > data.len() { break; }
        let cmd = data[offset];
        let tick = le_i32(data, offset + 1);
        offset += 5 + extra;

        match cmd {
            3 => {
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
            }
            7 => {
                packets.push(DemoPacketInfo { cmd, tick, payload_start: offset, payload_end: offset });
                break;
            }
            1 | 2 => {
                // democmdinfo(76) + in_seq(4) + out_seq(4) + length(4) = 88 bytes header
                if offset + 88 > data.len() { break; }
                let length = le_i32(data, offset + 84) as usize;
                let payload_start = offset + 88;
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

fn display_events_for_game(game_dir: &str) -> HashSet<&'static str> {
    let mut s = HashSet::new();
    // Common to all Source games
    s.insert("player_death");
    s.insert("player_spawn");
    s.insert("player_hurt");

    match game_dir {
        "tf" => {
            s.insert("player_teleported");
            s.insert("teamplay_round_start");
            s.insert("teamplay_round_active");
            s.insert("teamplay_round_win");
            s.insert("player_chargedeployed");
            s.insert("mvm_begin_wave");
            s.insert("mvm_wave_complete");
            s.insert("rocket_jump");
            s.insert("sticky_jump");
            s.insert("teamplay_flag_event");
            s.insert("teamplay_point_captured");
        }
        "cstrike" | "csgo" => {
            s.insert("bomb_planted");
            s.insert("bomb_defused");
            s.insert("bomb_exploded");
            s.insert("round_start");
            s.insert("round_end");
            s.insert("round_mvp");
            s.insert("weapon_fire");
        }
        "left4dead" | "left4dead2" => {
            s.insert("round_start");
            s.insert("round_end");
            s.insert("player_incapacitated");
            s.insert("player_ledge_grab");
            s.insert("survivor_rescued");
            s.insert("tank_spawn");
        }
        "portal" | "portal2" => {
            s.insert("portal_fired");
            s.insert("player_death");
            s.insert("challenge_mode_start_timer");
        }
        "hl2mp" => {
            s.insert("round_start");
            s.insert("round_end");
        }
        _ => {
            // Generic Source game events
            s.insert("round_start");
            s.insert("round_end");
        }
    }
    s
}

fn generate_html(dem_path: &Path, output_path: &Path) -> io::Result<()> {
    eprintln!("Reading {} ...", dem_path.file_name().unwrap_or_default().to_string_lossy());

    // Read demo file
    let mut file = File::open(dem_path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    let header = parse_header(&data).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "not a valid HL2DEMO file")
    })?;

    // Collect usercmds (SampledCmd) and all packet info
    let mut all_cmds: Vec<SampledCmd> = Vec::new();
    let mut last_pitch = 0.0f32;
    let mut last_yaw = 0.0f32;

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
    // viewangles instead of WriteFloat(32) — our parser would produce NaN/garbage.
    // Skip usercmd extraction for those demos; header/summary still work fine.
    let usercmd_supported = header.net_protocol > 7;
    if !usercmd_supported {
        eprintln!("  Note: net_protocol={} (old engine) — usercmd format unsupported, skipping input parsing", header.net_protocol);
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
                    let length = le_i32(&data, p + DEMOCMDINFO_SIZE + 8) as usize;
                    offset = p + DEMOCMDINFO_SIZE + 12 + length;
                }
                3 => { offset += pkt_hdr; }
                4 => {
                    let p = offset + pkt_hdr;
                    if p + 4 > data.len() { break; }
                    let length = le_i32(&data, p) as usize;
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
                        all_cmds.push(SampledCmd {
                            tick,
                            pitch: last_pitch,
                            yaw: last_yaw,
                            fwd: ucmd.forwardmove.unwrap_or(0.0),
                            side: ucmd.sidemove.unwrap_or(0.0),
                            btns: ucmd.buttons.unwrap_or(0),
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

    if let Some(ref schemas) = schemas {
        for &(tick, start, end) in &game_packet_ticks {
            let payload = &data[start..end];
            let evs = extract_events_from_payload(payload, tick, schemas, &display_ev);
            game_events.extend(evs);
        }
    }

    eprintln!(" {} found", game_events.len());

    // BSP
    let bsp_result = find_bsp_file(dem_path, &header.map_name);
    let (bsp_verts_b64, bsp_idx_b64, bsp_spawn) = match bsp_result {
        Some(ref bsp_path) => {
            eprintln!("  Found BSP: {}", bsp_path.file_name().unwrap_or_default().to_string_lossy());
            match extract_bsp(bsp_path) {
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
            }
        }
        None => {
            eprintln!("  No BSP found alongside demo");
            (String::new(), String::new(), [0.0f32; 3])
        }
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

    eprintln!(
        "  Life breaks: {}  sampled cmds: {}",
        life_breaks_sampled.len(), sampled_cmds.len()
    );

    // Build JSON
    let demo_name = dem_path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let meta_json = meta_to_json(&header.map_name, &header.client_name, header.playback_time, n);
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
    html = html.replace("__WORLD_POSITIONS__", "[]");
    html = html.replace("__BSP_VERTS__", &format!("\"{}\"", bsp_verts_b64));
    html = html.replace("__BSP_IDX__", &format!("\"{}\"", bsp_idx_b64));
    html = html.replace("__BSP_SPAWN__", &spawn_json);

    // Write output
    let mut out_file = File::create(output_path)?;
    out_file.write_all(html.as_bytes())?;

    let size_kb = html.len() as f64 / 1024.0;
    eprintln!(
        "HTML -> {}  ({:.1} KB)",
        output_path.display(), size_kb
    );

    Ok(())
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

    // Check for --html flag
    let html_idx = args.iter().position(|a| a == "--html");
    if let Some(idx) = html_idx {
        // Determine output path
        let output_path = if idx + 1 < args.len() && !args[idx + 1].starts_with("--") {
            PathBuf::from(&args[idx + 1])
        } else {
            Path::new(filename).with_extension("html")
        };
        let dem_path = Path::new(filename);
        return generate_html(dem_path, &output_path);
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
