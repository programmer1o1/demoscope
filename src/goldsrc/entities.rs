// GoldSrc entity decoder: the svc delta-compression protocol that turns the
// recorder's network message stream into per-player entity tracks. Driven by
// `extract_entities`, which walks each demo frame's svc blob through
// `decode_blob`. The demo-container parsing and recorder-camera path live in
// the parent `goldsrc` module; this module reuses its little-endian readers
// (`gi32`/`gf32`), the `is_netmsg_anchor` frame scanner, and the header offsets.
#![allow(dead_code)]

use super::{gf32, gi32, is_netmsg_anchor, GoldSrcMeta, GOLDSRC_HEADER_SIZE, NM_FIXED, NM_MSGLEN};

// ─────────────────────────────────────────────────────────────────────────────
// Other-player entity tracks.
//
// The recorder camera above comes from the per-frame `RefParams`. The *other*
// players live in the bit-packed svc message stream that each NetMsg frame
// carries at body+468 for `msg_length` bytes (the same blob the camera walker
// skips). That stream is GoldSrc's delta-compressed network protocol:
//
//   • svc_serverinfo (11)        - max players (player entity range = 1..=N)
//   • svc_deltadescription (14)  - registers a named field table for a struct
//                                  (entity_state_t, entity_state_player_t, …),
//                                  itself delta-encoded against a hardcoded
//                                  bootstrap "delta_description_t" meta-table
//   • svc_spawnbaseline (22)     - absolute baseline entity states
//   • svc_packetentities (40) /
//     svc_deltapacketentities(41)- per-entity field deltas; a present field
//                                  carries its absolute value, an absent one is
//                                  unchanged, so accumulating the latest value
//                                  of origin[0..2] / angles[0..2] per entity
//                                  reconstructs the trajectory.
//   • svc_updateuserinfo (13)    - player slot → "\name\…" infostring
//
// Field tables and the delta read logic mirror the GPL/LGPL reference parsers
// (khanghugo/dem, hlviewer.js). Because every NetMsg frame frames its blob with
// an explicit `msg_length`, a mis-sized svc message can only desync *within* one
// blob - the next frame resyncs - so the whole walk is panic- and desync-bounded.

use std::collections::HashMap;

/// Per-entity time-stamped samples decoded from the svc stream.
#[derive(Default)]
pub(crate) struct GoldSrcEntities {
    /// entity index → (time_seconds, x, y, z) in raw GoldSrc world coords.
    pub(crate) tracks: HashMap<u32, Vec<(f32, f32, f32, f32)>>,
    /// entity index → (time_seconds, yaw, pitch).
    pub(crate) yaws: HashMap<u32, Vec<(f32, f32, f32)>>,
    /// player slot entity index → display name.
    pub(crate) names: HashMap<u32, String>,
    /// recorder's own entity index, if known (RefParams `player_num`).
    pub(crate) primary: Option<u32>,
}

// Delta field-type flags (HLSDK DELTA_* / DT_*).
const DT_BYTE: u32 = 1;
const DT_SHORT: u32 = 1 << 1;
const DT_FLOAT: u32 = 1 << 2;
const DT_INTEGER: u32 = 1 << 3;
const DT_ANGLE: u32 = 1 << 4;
const DT_TIMEWINDOW_8: u32 = 1 << 5;
const DT_TIMEWINDOW_BIG: u32 = 1 << 6;
const DT_STRING: u32 = 1 << 7;
const DT_SIGNED: u32 = 1 << 31;

#[derive(Clone)]
struct FieldDesc {
    name: String,
    bits: u32,
    divisor: f32,
    flags: u32,
}
type Decoder = Vec<FieldDesc>;

enum DVal {
    Num(f64),
    Str(String),
}
impl DVal {
    fn num(&self) -> Option<f64> {
        match self {
            DVal::Num(n) => Some(*n),
            _ => None,
        }
    }
}

// ── minimal LSB-first bit reader over a byte slice (saturating) ──────────────
struct Bits<'a> {
    d: &'a [u8],
    pos: usize, // bit offset
    ovf: bool,
}
impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self {
        Bits { d, pos: 0, ovf: false }
    }
    fn bits(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            let byte = self.pos / 8;
            if byte >= self.d.len() {
                self.ovf = true;
                self.pos += (n - i) as usize;
                return v;
            }
            let bit = (self.d[byte] >> (self.pos % 8)) & 1;
            if i < 32 {
                v |= (bit as u32) << i;
            }
            self.pos += 1;
        }
        v
    }
    /// Advance past `n` bits without building a value (for fields wider than 32
    /// bits, e.g. 16-byte hashes). Sets `ovf` if it runs past the end.
    fn skip(&mut self, n: usize) {
        if self.pos + n > self.d.len() * 8 {
            self.ovf = true;
        }
        self.pos += n;
    }
    fn bit(&mut self) -> bool {
        self.bits(1) != 0
    }
    fn peek16(&self) -> u32 {
        let mut c = Bits { d: self.d, pos: self.pos, ovf: false };
        c.bits(16)
    }
    /// Null-terminated 8-bit string (terminator consumed, not included).
    fn read_str(&mut self) -> String {
        let mut out = Vec::new();
        loop {
            let b = self.bits(8) as u8;
            if b == 0 || self.ovf {
                break;
            }
            out.push(b);
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    fn consumed_bytes(&self) -> usize {
        self.pos.div_ceil(8)
    }
}

/// The hardcoded bootstrap meta-table that decodes every `svc_deltadescription`
/// field entry (HLSDK `g_MetaDelta`). Values verified against khanghugo/dem.
fn bootstrap_decoder() -> Decoder {
    let f = |name: &str, bits: u32, divisor: f32, flags: u32| FieldDesc {
        name: name.to_string(),
        bits,
        divisor,
        flags,
    };
    vec![
        f("flags", 32, 1.0, DT_INTEGER),
        f("name", 8, 1.0, DT_STRING),
        f("offset", 16, 1.0, DT_INTEGER),
        f("size", 8, 1.0, DT_INTEGER),
        f("bits", 8, 1.0, DT_INTEGER),
        f("divisor", 32, 4000.0, DT_FLOAT),
        f("preMultiplier", 32, 4000.0, DT_FLOAT),
    ]
}

/// Decode one delta field per its description (mirrors khanghugo/dem
/// `parse_delta_field`: integer types truncate-divide, floats real-divide).
fn parse_field(f: &FieldDesc, br: &mut Bits) -> DVal {
    let flags = f.flags;
    let signed = flags & DT_SIGNED != 0;
    let div = f.divisor;
    if flags & (DT_BYTE | DT_SHORT | DT_INTEGER) != 0 {
        let d = if div.abs() < 1.0 { 1 } else { div as i64 };
        let v = if signed {
            let sign: i64 = if br.bit() { -1 } else { 1 };
            let mag = br.bits(f.bits.saturating_sub(1)) as i64;
            sign * mag
        } else {
            br.bits(f.bits) as i64
        };
        DVal::Num((v / d) as f64)
    } else if flags & (DT_FLOAT | DT_TIMEWINDOW_8 | DT_TIMEWINDOW_BIG) != 0 {
        let v = if signed {
            let sign: f64 = if br.bit() { -1.0 } else { 1.0 };
            let mag = br.bits(f.bits.saturating_sub(1)) as f64;
            sign * mag
        } else {
            br.bits(f.bits) as f64
        };
        DVal::Num(v / div as f64)
    } else if flags & DT_ANGLE != 0 {
        let v = br.bits(f.bits) as f64;
        let mult = 360.0 / ((1u64 << f.bits.min(31)) as f64);
        DVal::Num(v * mult)
    } else if flags & DT_STRING != 0 {
        DVal::Str(br.read_str())
    } else {
        DVal::Num(0.0)
    }
}

/// Read a delta: a 3-bit mask-byte count, that many mask bytes, then the marked
/// fields in table order.
fn parse_delta(dec: &Decoder, br: &mut Bits) -> HashMap<String, DVal> {
    let mut res = HashMap::new();
    let nbytes = br.bits(3) as usize; // 0..=7
    let mut masks = [0u8; 8];
    for m in masks.iter_mut().take(nbytes) {
        *m = br.bits(8) as u8;
    }
    for i in 0..nbytes {
        for j in 0..8 {
            let idx = j + i * 8;
            if idx >= dec.len() {
                return res;
            }
            if masks[i] & (1 << j) != 0 {
                let d = &dec[idx];
                let v = parse_field(d, br);
                res.insert(d.name.clone(), v);
            }
            if br.ovf {
                return res;
            }
        }
    }
    res
}

/// Mutable decode state carried across every frame of both segments.
#[derive(Default)]
struct NetState {
    decoders: HashMap<String, Decoder>,
    max_client: u8,
    is_hltv: bool,
    custom_msg_size: HashMap<u8, i8>, // user-message id → fixed size (-1 = u8-prefixed)
    /// entity index → latest numeric field values (origin[*]/angles[*]/…).
    ent: HashMap<u16, HashMap<String, f64>>,
    out: GoldSrcEntities,
}

impl NetState {
    fn new() -> Self {
        let mut s = NetState::default();
        s.decoders
            .insert("delta_description_t".to_string(), bootstrap_decoder());
        s
    }
    fn dec(&self, name: &str) -> Option<&Decoder> {
        self.decoders.get(name)
    }
    /// Merge a parsed delta onto an entity's running state and, for player-slot
    /// entities, record an origin/angle sample.
    fn apply(&mut self, idx: u16, delta: HashMap<String, DVal>, time: f32) {
        let state = self.ent.entry(idx).or_default();
        for (k, v) in delta {
            if let DVal::Num(n) = v {
                state.insert(k, n);
            }
        }
        let is_player = idx >= 1 && idx as u8 <= self.max_client && self.max_client > 0;
        if !is_player {
            return;
        }
        let (ox, oy, oz) = (
            state.get("origin[0]").copied(),
            state.get("origin[1]").copied(),
            state.get("origin[2]").copied(),
        );
        if let (Some(x), Some(y), Some(z)) = (ox, oy, oz) {
            if x.is_finite() && y.is_finite() && z.is_finite() {
                self.out
                    .tracks
                    .entry(idx as u32)
                    .or_default()
                    .push((time, x as f32, y as f32, z as f32));
                let pitch = state.get("angles[0]").copied().unwrap_or(0.0) as f32;
                let yaw = state.get("angles[1]").copied().unwrap_or(0.0) as f32;
                self.out
                    .yaws
                    .entry(idx as u32)
                    .or_default()
                    .push((time, yaw, pitch));
            }
        }
    }
}

// ── byte-cursor helpers over the message blob (None = bail this blob) ────────
#[inline]
fn take(blob: &[u8], p: &mut usize, n: usize) -> Option<()> {
    if *p + n <= blob.len() {
        *p += n;
        Some(())
    } else {
        None
    }
}
#[inline]
fn cstr(blob: &[u8], p: &mut usize) -> Option<String> {
    let start = *p;
    while *p < blob.len() {
        if blob[*p] == 0 {
            let s = String::from_utf8_lossy(&blob[start..*p]).into_owned();
            *p += 1;
            return Some(s);
        }
        *p += 1;
    }
    None
}

/// Decode one NetMsg svc-message blob, updating `st`. Returns None on the first
/// unparseable message (the rest of this blob is skipped; the next frame is
/// unaffected because frames are length-framed).
fn decode_blob(blob: &[u8], st: &mut NetState, time: f32) -> Option<()> {
    let mut p = 0usize;
    while p < blob.len() {
        let t = blob[p];
        p += 1;
        match t {
            0 => return Some(()),                  // svc_bad
            1 | 19 | 27 | 28 | 30 | 42 => {}        // nop/damage/killedmonster/foundsecret/intermission/choke
            2 | 8 | 9 | 26 | 31 | 34 | 49 | 56 | 57 => {
                cstr(blob, &mut p)?;
            }
            3 => bit_msg(blob, &mut p, |br| {
                let n = br.bits(5);
                for _ in 0..n {
                    let _ev = br.bits(10);
                    let has_packet = br.bit();
                    if has_packet {
                        br.bits(11);
                        if br.bit() {
                            if let Some(d) = st.dec("event_t").cloned() {
                                parse_delta(&d, br);
                            } else {
                                br.ovf = true;
                            }
                        }
                    }
                    if br.bit() {
                        br.bits(16);
                    }
                }
            })?,
            4 => take(blob, &mut p, 4)?,            // version
            5 => take(blob, &mut p, 2)?,            // set_view
            6 => bit_msg(blob, &mut p, decode_sound)?,
            7 => take(blob, &mut p, 4)?,            // time
            10 => take(blob, &mut p, 6)?,           // set_angle
            11 => {
                // server_info: 3×i32, 16-byte hash, max_players, idx, dm,
                // 4×cstr, trailing u8.
                take(blob, &mut p, 12 + 16)?;
                let mp = *blob.get(p)?;
                st.max_client = mp;
                take(blob, &mut p, 3)?;
                for _ in 0..4 {
                    cstr(blob, &mut p)?;
                }
                take(blob, &mut p, 1)?;
            }
            12 => {
                take(blob, &mut p, 1)?;
                cstr(blob, &mut p)?;
            }
            13 => {
                // update_user_info: slot u8, id u32, infostring, 16-byte cdkey.
                let slot = *blob.get(p)? as u32;
                take(blob, &mut p, 1 + 4)?;
                let info = cstr(blob, &mut p)?;
                take(blob, &mut p, 16)?;
                if let Some(name) = info_value(&info, "name") {
                    if !name.is_empty() {
                        st.out.names.insert(slot + 1, name);
                    }
                }
            }
            14 => {
                // delta_description: name, total_fields u16, then bit-packed
                // field entries against the bootstrap meta-table.
                let name = cstr(blob, &mut p)?;
                let total = u16::from_le_bytes([*blob.get(p)?, *blob.get(p + 1)?]);
                take(blob, &mut p, 2)?;
                let boot = st.dec("delta_description_t")?.clone();
                let mut br = Bits::new(&blob[p..]);
                let mut decoder: Decoder = Vec::with_capacity(total as usize);
                for _ in 0..total {
                    let e = parse_delta(&boot, &mut br);
                    if br.ovf {
                        break;
                    }
                    let fname = match e.get("name") {
                        Some(DVal::Str(s)) => s.trim_end_matches('\0').to_string(),
                        _ => continue,
                    };
                    decoder.push(FieldDesc {
                        name: fname,
                        bits: e.get("bits").and_then(|v| v.num()).unwrap_or(0.0) as u32,
                        divisor: e.get("divisor").and_then(|v| v.num()).unwrap_or(1.0) as f32,
                        flags: e.get("flags").and_then(|v| v.num()).unwrap_or(0.0) as u32,
                    });
                }
                p += br.consumed_bytes();
                st.decoders
                    .insert(name.trim_end_matches('\0').to_string(), decoder);
            }
            15 => bit_msg(blob, &mut p, |br| {
                if st.is_hltv {
                    return;
                }
                if br.bit() {
                    br.bits(8);
                }
                match st.dec("clientdata_t").cloned() {
                    Some(d) => {
                        parse_delta(&d, br);
                    }
                    None => {
                        br.ovf = true;
                        return;
                    }
                }
                while br.bit() {
                    br.bits(6);
                    match st.dec("weapon_data_t").cloned() {
                        Some(d) => {
                            parse_delta(&d, br);
                        }
                        None => {
                            br.ovf = true;
                            return;
                        }
                    }
                }
            })?,
            16 => take(blob, &mut p, 2)?,           // stop_sound
            17 => bit_msg(blob, &mut p, |br| {
                while br.bit() {
                    br.bits(24); // player_id + ping + loss (3×8)
                }
            })?,
            18 => take(blob, &mut p, 11)?,          // particle
            20 => {
                // spawn_static: 17 fixed bytes; +3 if render-mode flag set.
                let extra = if *blob.get(p + 16)? != 0 { 3 } else { 0 };
                take(blob, &mut p, 17 + extra)?;
            }
            21 => bit_msg(blob, &mut p, |br| {
                br.bits(10);
                if let Some(d) = st.dec("event_t").cloned() {
                    parse_delta(&d, br);
                } else {
                    br.ovf = true;
                    return;
                }
                if br.bit() {
                    br.bits(16);
                }
            })?,
            22 => decode_spawnbaseline(blob, &mut p, st)?,
            23 => decode_temp_entity(blob, &mut p)?,
            24 | 25 => take(blob, &mut p, 1)?,      // set_pause / sign_on_num
            29 => take(blob, &mut p, 14)?,          // spawn_static_sound
            32 | 35 | 37 | 38 | 47 => take(blob, &mut p, 2)?,
            33 => {
                cstr(blob, &mut p)?;
                let mc = *blob.get(p)?;
                take(blob, &mut p, 1)?;
                for _ in 0..mc {
                    cstr(blob, &mut p)?;
                }
            }
            36 => {
                take(blob, &mut p, 1)?;
                cstr(blob, &mut p)?;
            }
            39 => {
                // new_user_msg: id u8, size i8, 16-byte name. Register the size
                // so later user messages can be sized correctly.
                let id = *blob.get(p)?;
                let size = *blob.get(p + 1)? as i8;
                take(blob, &mut p, 18)?;
                st.custom_msg_size.insert(id, size);
            }
            40 => decode_packet_entities(blob, &mut p, st, time)?,
            41 => decode_delta_packet_entities(blob, &mut p, st, time)?,
            43 => bit_msg(blob, &mut p, decode_resource_list)?,
            44 => {
                take(blob, &mut p, 97)?;
                cstr(blob, &mut p)?;
            }
            45 => take(blob, &mut p, 8)?,           // resource_request
            46 => {
                // customization: player u8, type u8, name cstr, index u16,
                // download_size u32, flags u8, +16-byte md5 if flags & 4.
                take(blob, &mut p, 2)?;
                cstr(blob, &mut p)?;
                let flags = *blob.get(p + 6)?; // after index(2) + download_size(4)
                take(blob, &mut p, 2 + 4 + 1)?;
                if flags & 4 != 0 {
                    take(blob, &mut p, 16)?;
                }
            }
            48 => take(blob, &mut p, 4)?,           // sound_fade
            50 => {
                take(blob, &mut p, 1)?;
                st.is_hltv = true;
            }
            51 => {
                let len = *blob.get(p)? as usize;
                take(blob, &mut p, len + 1)?; // length + command + (length-1)
            }
            52 => {
                cstr(blob, &mut p)?;
                take(blob, &mut p, 1)?;
            }
            53 => {
                let size = u16::from_le_bytes([*blob.get(p + 1)?, *blob.get(p + 2)?]) as usize;
                take(blob, &mut p, 3 + size)?;
            }
            54 => {
                cstr(blob, &mut p)?;
                take(blob, &mut p, 1)?;
            }
            55 => take(blob, &mut p, 4)?,           // time_scale
            58 => {
                take(blob, &mut p, 4)?;
                cstr(blob, &mut p)?;
            }
            59..=63 => return Some(()),             // reserved/bad — stop cleanly
            _ => {
                // User message (id > 63): fixed size if registered, else u8-prefixed.
                match st.custom_msg_size.get(&t) {
                    Some(&sz) if sz >= 0 => take(blob, &mut p, sz as usize)?,
                    _ => {
                        let len = *blob.get(p)? as usize;
                        take(blob, &mut p, 1 + len)?;
                    }
                }
            }
        }
    }
    Some(())
}

/// Run a bit-packed message via `f`, then advance the byte cursor to the next
/// byte boundary (every svc message starts byte-aligned).
fn bit_msg(blob: &[u8], p: &mut usize, f: impl FnOnce(&mut Bits)) -> Option<()> {
    let mut br = Bits::new(blob.get(*p..)?);
    f(&mut br);
    if br.ovf {
        return None;
    }
    take(blob, p, br.consumed_bytes())
}

fn decode_sound(br: &mut Bits) {
    let flags = br.bits(9);
    if flags & 1 != 0 {
        br.bits(8);
    }
    if flags & 2 != 0 {
        br.bits(8);
    }
    br.bits(3); // channel
    br.bits(11); // entity
    if flags & 4 != 0 {
        br.bits(16);
    } else {
        br.bits(8);
    }
    let (hx, hy, hz) = (br.bit(), br.bit(), br.bit());
    for has in [hx, hy, hz] {
        if has {
            read_sound_origin(br);
        }
    }
    if flags & 8 != 0 {
        br.bits(8);
    }
}
fn read_sound_origin(br: &mut Bits) {
    let int_flag = br.bit();
    let frac_flag = br.bit();
    if int_flag || frac_flag {
        br.bit(); // sign
    }
    if int_flag {
        br.bits(12);
    }
    if frac_flag {
        br.bits(3);
    }
}

fn decode_resource_list(br: &mut Bits) {
    let count = br.bits(12);
    for _ in 0..count {
        br.bits(4); // type
        br.read_str(); // name
        br.bits(12); // index
        br.bits(24); // size
        let flags = br.bits(3);
        if flags & 4 != 0 {
            br.skip(128); // md5 (16 bytes)
        }
        if br.bit() {
            br.skip(256); // extra info (32 bytes)
        }
        if br.ovf {
            return;
        }
    }
    if br.bit() {
        while br.bit() {
            if br.bit() {
                br.bits(5);
            } else {
                br.bits(10);
            }
            if br.ovf {
                return;
            }
        }
    }
}

fn decode_spawnbaseline(blob: &[u8], p: &mut usize, st: &mut NetState) -> Option<()> {
    let mut br = Bits::new(blob.get(*p..)?);
    let max = st.max_client as u16;
    let ent_t = st.dec("entity_state_t").cloned();
    let player_t = st.dec("entity_state_player_t").cloned();
    let custom_t = st.dec("custom_entity_state_t").cloned();
    while br.peek16() != 0xffff {
        let idx = br.bits(11) as u16;
        let between = idx > 0 && idx <= max;
        let type_ = br.bits(2);
        let dec = if type_ & 1 != 0 {
            if between { player_t.as_ref() } else { ent_t.as_ref() }
        } else {
            custom_t.as_ref()
        };
        match dec {
            Some(d) => {
                let delta = parse_delta(d, &mut br);
                st.apply(idx, delta, 0.0);
            }
            None => return None,
        }
        if br.ovf {
            return None;
        }
    }
    br.bits(16); // footer
    let extra = br.bits(6);
    if let Some(d) = ent_t.as_ref() {
        for _ in 0..extra {
            parse_delta(d, &mut br);
            if br.ovf {
                break;
            }
        }
    }
    if br.ovf {
        return None;
    }
    take(blob, p, br.consumed_bytes())
}

fn decode_packet_entities(blob: &[u8], p: &mut usize, st: &mut NetState, time: f32) -> Option<()> {
    let mut br = Bits::new(blob.get(*p..)?);
    let max = st.max_client as u16;
    let ent_t = st.dec("entity_state_t").cloned();
    let player_t = st.dec("entity_state_player_t").cloned();
    let custom_t = st.dec("custom_entity_state_t").cloned();
    br.bits(16); // entity_count
    let mut idx: u16 = 0;
    loop {
        if br.peek16() == 0 {
            br.bits(16);
            break;
        }
        if br.bit() {
            idx += 1;
        } else if br.bit() {
            idx = br.bits(11) as u16;
        } else {
            idx += br.bits(6) as u16;
        }
        let has_custom = br.bit();
        if br.bit() {
            br.bits(6); // baseline index
        }
        let between = idx > 0 && idx <= max;
        let dec = if between {
            player_t.as_ref()
        } else if has_custom {
            custom_t.as_ref()
        } else {
            ent_t.as_ref()
        };
        match dec {
            Some(d) => {
                let delta = parse_delta(d, &mut br);
                st.apply(idx, delta, time);
            }
            None => return None,
        }
        if br.ovf {
            return None;
        }
    }
    take(blob, p, br.consumed_bytes())
}

fn decode_delta_packet_entities(
    blob: &[u8],
    p: &mut usize,
    st: &mut NetState,
    time: f32,
) -> Option<()> {
    let mut br = Bits::new(blob.get(*p..)?);
    let max = st.max_client as u16;
    let ent_t = st.dec("entity_state_t").cloned();
    let player_t = st.dec("entity_state_player_t").cloned();
    let custom_t = st.dec("custom_entity_state_t").cloned();
    br.bits(16); // entity_count
    br.bits(8); // delta_sequence
    let mut idx: u16 = 0;
    loop {
        if br.peek16() == 0 {
            br.bits(16);
            break;
        }
        let remove = br.bit();
        if br.bit() {
            idx = br.bits(11) as u16;
        } else {
            idx += br.bits(6) as u16;
        }
        if remove {
            continue;
        }
        let has_custom = br.bit();
        let between = idx > 0 && idx <= max;
        let dec = if between {
            player_t.as_ref()
        } else if has_custom {
            custom_t.as_ref()
        } else {
            ent_t.as_ref()
        };
        match dec {
            Some(d) => {
                let delta = parse_delta(d, &mut br);
                st.apply(idx, delta, time);
            }
            None => return None,
        }
        if br.ovf {
            return None;
        }
    }
    take(blob, p, br.consumed_bytes())
}

/// svc_temp_entity (23): one type byte then a per-type fixed/variable payload.
fn decode_temp_entity(blob: &[u8], p: &mut usize) -> Option<()> {
    let kind = *blob.get(*p)?;
    *p += 1;
    let fixed = |n: usize| n;
    let n = match kind {
        0 => fixed(24),  // beam_points
        1 => 20,
        2 => 6,
        3 => 11,
        4 => 6,
        5 => 10,
        6 => 12,
        7 => 17,
        8 => 16,
        9 => 6,
        10 => 6,
        11 => 6,
        12 => 8,
        13 => {
            // bsp_decal: 8 + i16 entity_index, +2 if entity_index != 0
            let ei = i16::from_le_bytes([*blob.get(*p + 8)?, *blob.get(*p + 9)?]);
            if ei != 0 { 12 } else { 10 }
        }
        14 => 9,
        15 => 19,
        17 => 10,
        18 => 16,
        19 | 20 | 21 => 24,
        22 => 10,
        23 => 11,
        24 => 16,
        25 => 19,
        27 => 12,
        28 => 16,
        29 => {
            // text_message: 18 fixed, +2 if effect (byte at +5) != 0, then cstr
            let effect = *blob.get(*p + 5)?;
            let mut q = *p + 18 + if effect != 0 { 2 } else { 0 };
            // null-terminated message
            while q < blob.len() && blob[q] != 0 {
                q += 1;
            }
            if q >= blob.len() {
                return None;
            }
            *p = q + 1;
            return Some(());
        }
        30 | 31 => 17,
        99 => 2,
        100 => 10,
        101 => 14,
        102 => 12,
        103 => 14,
        104 => 9,
        105 => 5,
        106 => 17,
        107 => 13,
        108 => 24,
        109 => 9,
        110 => 17,
        111 => 7,
        112 => 10,
        113 => 10,
        114 => 19,
        115 => 12,
        116 | 117 => 7,
        118 => 9,
        119 => 16,
        120 => 18,
        121 => 5,
        122 => 10,
        123 => 9,
        124 => 7,
        125 => 1,
        126 => 18,
        127 => 15,
        _ => return None,
    };
    take(blob, p, n)
}

/// Pull a value out of a `\key\value\…` infostring.
fn info_value(info: &str, key: &str) -> Option<String> {
    let parts: Vec<&str> = info.trim_start_matches('\\').split('\\').collect();
    let mut i = 0;
    while i + 1 < parts.len() {
        if parts[i] == key {
            return Some(parts[i + 1].to_string());
        }
        i += 2;
    }
    None
}

/// Walk both demo segments (signon + gameplay) and decode the other-player
/// entity tracks out of the svc message stream. The frame stepping mirrors
/// `extract_camera`'s resync logic; the only addition is decoding each NetMsg
/// blob instead of skipping it.
pub(crate) fn extract_entities(data: &[u8], meta: &GoldSrcMeta) -> GoldSrcEntities {
    let mut st = NetState::new();
    if meta.play_offset == 0 {
        return st.out;
    }
    // Start at the signon (right after the 544-byte header) so svc_serverinfo /
    // svc_deltadescription / svc_updateuserinfo are seen before gameplay.
    let start = GOLDSRC_HEADER_SIZE;
    let end = (meta.play_offset + meta.play_length).min(data.len());
    let mut p = start;
    let mut last_t = -1.0f32;
    let mut iters = 0usize;
    let resync = |from: usize| -> Option<usize> {
        let mut r = from + 1;
        let limit = (from + 262_144).min(end.saturating_sub(9));
        while r < limit {
            if is_netmsg_anchor(data, r) {
                return Some(r);
            }
            r += 1;
        }
        None
    };

    while p + 9 <= end {
        iters += 1;
        if iters > 2_000_000 {
            break;
        }
        let t = data[p];
        let time = gf32(data, p + 1);
        if t > 9 || !(time.is_finite() && time >= last_t - 1.0 && time < 1000.0) {
            match resync(p) {
                Some(np) => {
                    p = np;
                    continue;
                }
                None => break,
            }
        }
        last_t = last_t.max(time);
        let body = p + 9;
        match t {
            0 | 1 => {
                let ml = gi32(data, body + NM_MSGLEN);
                if !(0..=65536).contains(&ml) {
                    match resync(p) {
                        Some(np) => {
                            p = np;
                            continue;
                        }
                        None => break,
                    }
                }
                let blob_start = body + NM_FIXED;
                let blob_end = (blob_start + ml as usize).min(data.len());
                if blob_start <= blob_end {
                    let _ = decode_blob(&data[blob_start..blob_end], &mut st, time);
                }
                p = body + NM_FIXED + ml as usize;
            }
            2 => p = body,
            3 => p = body + 64,
            4 => p = body + 32,
            5 => p = body, // NextSection: continue into the next segment
            6 => p = body + 76,
            7 => p = body + 8,
            8 => {
                let sl = gi32(data, body + 4);
                if !(0..=8192).contains(&sl) {
                    match resync(p) {
                        Some(np) => { p = np; continue; }
                        None => break,
                    }
                }
                p = body + 24 + sl as usize;
            }
            9 => {
                let ln = gi32(data, body);
                if !(0..=2_000_000).contains(&ln) {
                    match resync(p) {
                        Some(np) => { p = np; continue; }
                        None => break,
                    }
                }
                p = body + 4 + ln as usize;
            }
            _ => break,
        }
    }

    // Recorder's own entity (RefParams `player_num`), if it lands on a slot.
    st.out.primary = st
        .out
        .tracks
        .keys()
        .copied()
        .min();
    st.out
}
