// GoldSrc (Half-Life 1 / CS 1.6 / DoD / Condition Zero) `HLDEMO` container.
//
// GoldSrc is the Quake-derived engine that predates the Source `HL2DEMO`
// format the rest of the crate decodes. Its demo is a directory-of-segments
// container (reverse-engineered + verified byte-for-byte against a real
// Condition Zero demo: cs_assault, 7916 frames, directory math exact):
//
//   [0]      544-byte header  (magic, protocols, map, game dir, dir offset)
//   [544..]  segment 0 "LOADING"   (signon)
//   [..]     segment 1 "Playback"  (gameplay - the bulk)
//   [tail]   directory  (entry count + N × 92-byte entries)
//
//   Header layout (offsets):
//     0   char  magic[8]      "HLDEMO\0\0"
//     8   i32   demo_protocol (5)
//     12  i32   net_protocol  (48 = Steam HL / CS 1.6 / CZ era)
//     16  char  map_name[260]
//     276 char  game_dir[260]
//     536 i32   (unused/junk)
//     540 i32   directory_offset
//
//   Directory entry (92 bytes): type(i32) desc[64] flags(i32) cd_track(i32)
//     track_time(f32) frame_count(i32) offset(i32) length(i32)
//
// Each segment is a stream of frames. A frame is a 9-byte header
// (type:u8, time:f32, frame:i32) followed by type-specific data. The frame
// layout (and the NetMsg `RefParams`/`UserCmd`/`MoveVars` field order this
// module relies on) is transcribed from the open-source GoldSrc demo parser
// `hldemo` (YaLTeR/hldemo-rs) and validated against the CZ demo:
//
//   type 0/1 NetMsg : view info + svc data. body = 468 + msg_length, with the
//                     recorder eye `RefParams.vieworg` at body+4 and view angles
//                     at body+16; `msg_length` (i32) sits at body+464. (This
//                     engine writes 6 netchan seq ints, not the 7 some builds
//                     use, hence the 464 offset - solved against the sample.)
//   type 2  DemoStart    : no body
//   type 3  ConsoleCommand: 64 bytes (fixed char[64])
//   type 4  ClientData   : 32 bytes (origin vec3, viewangles vec3, weaponbits, fov)
//   type 5  NextSection  : no body - terminates the segment
//   type 6  Event        : 76 bytes
//   type 7  WeaponAnim   : 8 bytes
//   type 8  Sound        : 24 + sample_length (i32 length at body+4)
//   type 9  DemoBuffer   : 4 + buffer_length (i32 length at body+0)
//
// The recorder camera path (eye origin + view angles) comes straight out of the
// NetMsg `RefParams`, so GoldSrc demos render a real POV trajectory in the
// viewer - no IDA needed; the structs are public. A handful of frames per demo
// carry a length quirk that desyncs the byte cursor; the walker resyncs by
// scanning forward to the next `RefParams`-validated NetMsg (≈26 / 24k frames on
// the CZ sample), so the trajectory stays complete.

use super::util::bytes::{le_f32, le_i32, read_cstring};

// The svc delta-compression entity decoder (per-player tracks) lives in its
// own submodule; its public surface is re-exported so callers keep using
// `goldsrc::extract_entities` / `goldsrc::GoldSrcEntities` unchanged.
pub(crate) mod entities;
pub(crate) use entities::{extract_entities, GoldSrcEntities};

pub(crate) const GOLDSRC_MAGIC: &[u8; 8] = b"HLDEMO\0\0";
const GOLDSRC_HEADER_SIZE: usize = 544;
const DIR_ENTRY_SIZE: usize = 92;

// NetMsg body layout (offsets from the start of the frame body = header+9).
const NM_VIEWORG: usize = 4; // RefParams.vieworg vec3 (recorder eye position)
const NM_VIEWANGLES: usize = 16; // RefParams.viewangles vec3 (pitch, yaw, roll)
const NM_MSGLEN: usize = 464; // i32 message length; svc data follows at +468
const NM_FIXED: usize = NM_MSGLEN + 4; // bytes before the variable svc message
// RefParams validation anchors (offsets from body), used to resync after a
// desync: these fields have tight expected ranges on any real demo.
const NM_MAXCLIENTS: usize = 4 + 172;
const NM_VIEWENTITY: usize = 4 + 176;
const NM_DEMOPLAYBACK: usize = 4 + 188;
const NM_HARDWARE: usize = 4 + 192;

/// True if the leading bytes are a GoldSrc `HLDEMO` container.
pub(crate) fn is_goldsrc(data: &[u8]) -> bool {
    data.len() >= 8 && &data[0..8] == GOLDSRC_MAGIC
}

#[derive(Debug, Default)]
pub(crate) struct GoldSrcMeta {
    pub(crate) map_name: String,
    pub(crate) game_dir: String,
    pub(crate) demo_protocol: i32,
    pub(crate) net_protocol: i32,
    /// Playback duration in seconds (from the gameplay directory entry).
    pub(crate) duration: f32,
    /// Number of recorded server frames in the gameplay segment.
    pub(crate) frame_count: i32,
    /// File offset + byte length of the gameplay ("Playback") segment.
    pub(crate) play_offset: usize,
    pub(crate) play_length: usize,
}

/// One recorder camera sample: (time_seconds, x, y, z, pitch, yaw) in raw
/// GoldSrc world coordinates (z = up), pulled from a NetMsg `RefParams`.
pub(crate) type CamSample = (f32, f32, f32, f32, f32, f32);

// ── bounds-safe little-endian readers (return 0 past the end) ────────────────
#[inline]
fn gi32(d: &[u8], o: usize) -> i32 {
    if o + 4 <= d.len() { le_i32(d, o) } else { 0 }
}
#[inline]
fn gf32(d: &[u8], o: usize) -> f32 {
    if o + 4 <= d.len() { le_f32(d, o) } else { 0.0 }
}

/// Parse the GoldSrc header + directory into display metadata. Returns None if
/// the magic is wrong or the directory offset/entries don't validate.
pub(crate) fn parse(data: &[u8]) -> Option<GoldSrcMeta> {
    if data.len() < GOLDSRC_HEADER_SIZE || !is_goldsrc(data) {
        return None;
    }
    let mut m = GoldSrcMeta {
        demo_protocol: le_i32(data, 8),
        net_protocol: le_i32(data, 12),
        map_name: read_cstring(data, 16, 260),
        game_dir: read_cstring(data, 276, 260),
        ..Default::default()
    };

    // Directory offset lives at 540 (the i32 at 536 is an uninitialized field
    // in the recorder, not the offset - verified on the CZ sample).
    let dir_off = le_i32(data, 540);
    if dir_off <= 0 {
        return Some(m); // header is still usable even if the directory is odd
    }
    let dir_off = dir_off as usize;
    if dir_off + 4 > data.len() {
        return Some(m);
    }
    let count = le_i32(data, dir_off);
    if count <= 0 || count > 1024 {
        return Some(m);
    }
    // Pick the gameplay segment: the entry with the most frames (the "Playback"
    // block; the "LOADING" signon segment reports 0 frames).
    let mut best_frames = 0i32;
    for i in 0..count as usize {
        let e = dir_off + 4 + i * DIR_ENTRY_SIZE;
        if e + DIR_ENTRY_SIZE > data.len() {
            break;
        }
        let track_time = le_f32(data, e + 76);
        let frames = le_i32(data, e + 80);
        let offset = le_i32(data, e + 84);
        let length = le_i32(data, e + 88);
        if frames > best_frames {
            best_frames = frames;
            m.frame_count = frames;
            m.duration = track_time;
            if offset > 0 && length > 0 && (offset as usize) < data.len() {
                m.play_offset = offset as usize;
                m.play_length = (length as usize).min(data.len() - offset as usize);
            }
        }
    }
    Some(m)
}

/// True if `p` looks like a real NetMsg frame header, judged by the tight
/// expected ranges of several `RefParams` fields. Used to resync the walk.
fn is_netmsg_anchor(d: &[u8], p: usize) -> bool {
    if p + 9 + NM_HARDWARE + 4 > d.len() {
        return false;
    }
    if d[p] > 1 {
        return false; // NetMsg is frame type 0 or 1
    }
    let t = gf32(d, p + 1);
    if !(t.is_finite() && (-1.0..200.0).contains(&t)) {
        return false;
    }
    let body = p + 9;
    let maxclients = gi32(d, body + NM_MAXCLIENTS);
    let hardware = gi32(d, body + NM_HARDWARE);
    let viewentity = gi32(d, body + NM_VIEWENTITY);
    let demoplayback = gi32(d, body + NM_DEMOPLAYBACK);
    (1..=32).contains(&maxclients)
        && (0..=1).contains(&hardware)
        && (0..=64).contains(&viewentity)
        && (0..=1).contains(&demoplayback)
}

/// Walk the gameplay segment and pull the recorder camera path out of the
/// NetMsg `RefParams`. Resyncs past the occasional length-quirk frame.
pub(crate) fn extract_camera(data: &[u8], meta: &GoldSrcMeta) -> Vec<CamSample> {
    let mut cam = Vec::new();
    let start = meta.play_offset;
    if start == 0 {
        return cam;
    }
    let end = (start + meta.play_length).min(data.len());
    let mut p = start;
    let mut last_t = -1.0f32;
    let mut iters = 0usize;
    // A frame whose header is implausible (or a NetMsg with a bad length) means
    // the byte cursor desynced; scan forward to the next validated NetMsg.
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
        if iters > 500_000 {
            break; // hard backstop against a pathological loop
        }
        let t = data[p];
        let time = gf32(data, p + 1);
        // Header sanity: valid type byte and a monotone-ish, in-range time.
        if t > 9 || !(time.is_finite() && time >= last_t - 1.0 && time < 200.0) {
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
                // NetMsg: capture the recorder eye + view angles, then skip the
                // fixed info block + the variable svc message.
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
                let x = gf32(data, body + NM_VIEWORG);
                let y = gf32(data, body + NM_VIEWORG + 4);
                let z = gf32(data, body + NM_VIEWORG + 8);
                let pitch = gf32(data, body + NM_VIEWANGLES);
                let yaw = gf32(data, body + NM_VIEWANGLES + 4);
                if x.is_finite() && y.is_finite() && z.is_finite()
                    && x.abs() < 100_000.0 && y.abs() < 100_000.0 && z.abs() < 100_000.0
                {
                    cam.push((time, x, y, z, pitch, yaw));
                }
                p = body + NM_FIXED + ml as usize;
            }
            2 => p = body,                                  // DemoStart
            3 => p = body + 64,                             // ConsoleCommand
            4 => p = body + 32,                             // ClientData
            5 => break,                                     // NextSection - end
            6 => p = body + 76,                             // Event
            7 => p = body + 8,                              // WeaponAnim
            8 => {
                // Sound: channel(4) + length_bytes(4+N) + 16
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
                // DemoBuffer: length_bytes(4+N)
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
    cam
}

