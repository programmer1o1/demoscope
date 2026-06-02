// Quake 1 demo decoder.
//
// NetQuake (.dem, protocol 15): the original single/listen-server demo format.
// Container: an ASCII CD-track line terminated by '\n', then a sequence of
// blocks - `i32 len` + 12 bytes of recorder viewangles + `len` bytes of message.
// Messages are plain byte/short oriented (no compression). Entity positions
// arrive as delta-compressed updates (the high bit of the command byte flags an
// update); the recorder's view entity is named by svc_setview. Refs: original
// Quake source (cl_parse.c CL_ParseServerMessage / CL_ParseUpdate, common.c MSG_*).
//
// QuakeWorld (.qwd / .mvd): a different, extension-negotiated protocol (the MVD
// test file on hand carries FTE protocol extensions). It is NOT vanilla NetQuake
// and is left as a documented gap rather than a half-correct decode.

use std::collections::HashMap;
use std::error::Error;

use super::{ByteReader, QuakeDemo, QuakeMeta, TrackBuilder};

// svc ops (protocol 15)
const SVC_BAD: u8 = 0;
const SVC_NOP: u8 = 1;
const SVC_DISCONNECT: u8 = 2;
const SVC_UPDATESTAT: u8 = 3;
const SVC_VERSION: u8 = 4;
const SVC_SETVIEW: u8 = 5;
const SVC_SOUND: u8 = 6;
const SVC_TIME: u8 = 7;
const SVC_PRINT: u8 = 8;
const SVC_STUFFTEXT: u8 = 9;
const SVC_SETANGLE: u8 = 10;
const SVC_SERVERINFO: u8 = 11;
const SVC_LIGHTSTYLE: u8 = 12;
const SVC_UPDATENAME: u8 = 13;
const SVC_UPDATEFRAGS: u8 = 14;
const SVC_CLIENTDATA: u8 = 15;
const SVC_STOPSOUND: u8 = 16;
const SVC_UPDATECOLORS: u8 = 17;
const SVC_PARTICLE: u8 = 18;
const SVC_DAMAGE: u8 = 19;
const SVC_SPAWNSTATIC: u8 = 20;
const SVC_SPAWNBASELINE: u8 = 22;
const SVC_TEMP_ENTITY: u8 = 23;
const SVC_SETPAUSE: u8 = 24;
const SVC_SIGNONNUM: u8 = 25;
const SVC_CENTERPRINT: u8 = 26;
const SVC_KILLEDMONSTER: u8 = 27;
const SVC_FOUNDSECRET: u8 = 28;
const SVC_SPAWNSTATICSOUND: u8 = 29;
const SVC_INTERMISSION: u8 = 30;
const SVC_FINALE: u8 = 31;
const SVC_CDTRACK: u8 = 32;
const SVC_SELLSCREEN: u8 = 33;
const SVC_CUTSCENE: u8 = 34;

// Entity-update bit flags (the command byte's high bit, U_SIGNAL=0x80, marks an
// update; bits 0..6 plus an optional second byte select the present fields).
const U_MOREBITS: u32 = 1 << 0;
const U_ORIGIN1: u32 = 1 << 1;
const U_ORIGIN2: u32 = 1 << 2;
const U_ORIGIN3: u32 = 1 << 3;
const U_ANGLE2: u32 = 1 << 4;
const U_FRAME: u32 = 1 << 6;
const U_ANGLE1: u32 = 1 << 8;
const U_ANGLE3: u32 = 1 << 9;
const U_MODEL: u32 = 1 << 10;
const U_COLORMAP: u32 = 1 << 11;
const U_SKIN: u32 = 1 << 12;
const U_EFFECTS: u32 = 1 << 13;
const U_LONGENTITY: u32 = 1 << 14;

// svc_clientdata SU_ flags
const SU_VIEWHEIGHT: u32 = 1 << 0;
const SU_IDEALPITCH: u32 = 1 << 1;
const SU_PUNCH1: u32 = 1 << 2;
const SU_VELOCITY1: u32 = 1 << 5;
const SU_ITEMS: u32 = 1 << 9;
const SU_WEAPONFRAME: u32 = 1 << 12;
const SU_ARMOR: u32 = 1 << 13;
const SU_WEAPON: u32 = 1 << 14;

#[derive(Clone, Copy, Default)]
struct Ent {
    origin: [f32; 3],
    angles: [f32; 3],
    seen: bool,
}

#[inline]
fn read_angle_i8(m: &mut ByteReader) -> f32 {
    m.read_i8() as f32 * (360.0 / 256.0)
}

pub fn parse_netquake(data: &[u8]) -> Result<QuakeDemo, Box<dyn Error>> {
    let dbg = std::env::var("DUMP_QUAKE").is_ok();
    let mut r = ByteReader::new(data);

    // Leading CD-track line: ASCII digits terminated by '\n'.
    while !r.eof() {
        if r.read_u8() == b'\n' {
            break;
        }
    }

    let mut tb = TrackBuilder::default();
    let mut ents: HashMap<u16, Ent> = HashMap::new();
    let mut names: HashMap<u16, String> = HashMap::new();
    let mut map = String::new();
    let mut view_entity: i32 = -1;
    let mut time: f32 = 0.0;
    let mut max_tick: i32 = 0;
    let mut nframes: usize = 0;

    loop {
        if r.remaining() < 4 {
            break;
        }
        let len = r.read_i32();
        // recorder viewangles (3 floats) = the playback camera [pitch, yaw, roll]
        let va_pitch = r.read_f32();
        let va_yaw = r.read_f32();
        let _va_roll = r.read_f32();
        if len < 0 {
            break;
        }
        let block = r.read_bytes(len as usize);
        if r.overflow {
            break;
        }
        nframes += 1;
        let mut m = ByteReader::new(&block);

        while !m.eof() && !m.overflow {
            let cmd = m.read_u8();
            if cmd & 0x80 != 0 {
                parse_update(&mut m, (cmd & 0x7f) as u32, time, &mut ents, &mut tb);
                continue;
            }
            match cmd {
                SVC_BAD | SVC_DISCONNECT => {
                    break;
                }
                SVC_NOP | SVC_KILLEDMONSTER | SVC_FOUNDSECRET | SVC_INTERMISSION
                | SVC_SELLSCREEN => {}
                SVC_UPDATESTAT => {
                    m.read_u8();
                    m.read_i32();
                }
                SVC_VERSION => {
                    m.read_i32();
                }
                SVC_SETVIEW => {
                    view_entity = m.read_i16() as i32;
                }
                SVC_SOUND => {
                    let mask = m.read_u8() as u32;
                    if mask & 0x01 != 0 { m.read_u8(); } // volume
                    if mask & 0x02 != 0 { m.read_u8(); } // attenuation
                    m.read_i16(); // channel/ent
                    m.read_u8(); // sound num
                    m.skip(6); // position (3 coords)
                }
                SVC_TIME => {
                    time = m.read_f32();
                    let tick = (time * 1000.0) as i32;
                    max_tick = max_tick.max(tick);
                }
                SVC_PRINT | SVC_STUFFTEXT | SVC_CENTERPRINT | SVC_FINALE | SVC_CUTSCENE => {
                    let _ = m.read_string();
                }
                SVC_SETANGLE => {
                    // recorder view angles (3 angles); the block float angles are
                    // denser, so we keep those for the POV camera.
                    m.skip(3);
                }
                SVC_SERVERINFO => {
                    let _protocol = m.read_i32();
                    let _maxclients = m.read_u8();
                    let _gametype = m.read_u8();
                    map = m.read_string();
                    // model precache (until empty), then sound precache (until empty)
                    loop {
                        let s = m.read_string();
                        if s.is_empty() || m.overflow { break; }
                    }
                    loop {
                        let s = m.read_string();
                        if s.is_empty() || m.overflow { break; }
                    }
                }
                SVC_LIGHTSTYLE => {
                    m.read_u8();
                    let _ = m.read_string();
                }
                SVC_UPDATENAME => {
                    let slot = m.read_u8();
                    let name = m.read_string();
                    names.insert(slot as u16 + 1, name); // player ent = slot+1
                }
                SVC_UPDATEFRAGS => {
                    m.read_u8();
                    m.read_i16();
                }
                SVC_CLIENTDATA => parse_clientdata(&mut m),
                SVC_STOPSOUND => {
                    m.read_i16();
                }
                SVC_UPDATECOLORS => {
                    m.read_u8();
                    m.read_u8();
                }
                SVC_PARTICLE => {
                    m.skip(6); // origin (3 coords)
                    m.skip(3); // direction (3 chars)
                    m.read_u8(); // count
                    m.read_u8(); // color
                }
                SVC_DAMAGE => {
                    m.read_u8(); // armor
                    m.read_u8(); // blood
                    m.skip(6); // position
                }
                SVC_SPAWNSTATIC => {
                    read_baseline_body(&mut m);
                }
                SVC_SPAWNBASELINE => {
                    let num = m.read_i16() as u16;
                    let (origin, angles) = read_baseline_body(&mut m);
                    let st = Ent { origin, angles, seen: true };
                    ents.insert(num, st);
                }
                SVC_TEMP_ENTITY => parse_temp_entity(&mut m),
                SVC_SETPAUSE | SVC_SIGNONNUM => {
                    m.read_u8();
                }
                SVC_SPAWNSTATICSOUND => {
                    m.skip(6); // position
                    m.read_u8(); // sound num
                    m.read_u8(); // volume
                    m.read_u8(); // attenuation
                }
                SVC_CDTRACK => {
                    m.read_u8();
                    m.read_u8();
                }
                _ => {
                    if dbg {
                        eprintln!("[q1] unknown svc {cmd} @ block pos {}", m.pos);
                    }
                    break;
                }
            }
        }
        // Recorder POV camera angles for this frame, stamped at the block's time.
        let tick = (time * 1000.0) as i32;
        if va_pitch != 0.0 || va_yaw != 0.0 {
            tb.view_angles.push((tick, va_pitch, va_yaw));
        }
    }

    // The recorder camera path doubles as the primary track if the view entity
    // has no entity samples.
    if view_entity >= 0 {
        tb.primary = Some(view_entity as u32);
    }
    for (eid, name) in names {
        tb.name(eid as u32, name);
    }

    let tick_rate = 1000.0;
    let duration = max_tick as f32 / tick_rate;
    let total_samples: usize = tb.tracks.values().map(|v| v.len()).sum();
    eprintln!(
        "  [Quake1/NetQuake] map={} players={} entities={} samples={} frames={} (note: NetQuake path not verified against a local demo)",
        map,
        tb.names.len(),
        tb.tracks.len(),
        total_samples,
        nframes
    );

    let meta = QuakeMeta {
        map: map.clone(),
        server: String::new(),
        client: tb.primary.and_then(|p| tb.names.get(&p).cloned()).unwrap_or_default(),
        game: "quake1".to_string(),
        protocol: 15,
        duration,
        tick_rate,
        ncmds: nframes,
    };
    let mpd = tb.build(map, String::new(), duration, max_tick);
    Ok(QuakeDemo { meta, mpd })
}

/// CL_ParseUpdate: a delta entity update. The high command bit is already
/// stripped; `bits` holds the low 7 plus an optional extension byte.
fn parse_update(
    m: &mut ByteReader,
    mut bits: u32,
    time: f32,
    ents: &mut HashMap<u16, Ent>,
    tb: &mut TrackBuilder,
) {
    if bits & U_MOREBITS != 0 {
        bits |= (m.read_u8() as u32) << 8;
    }
    let num = if bits & U_LONGENTITY != 0 {
        m.read_i16() as u16
    } else {
        m.read_u8() as u16
    };
    let mut st = ents.get(&num).copied().unwrap_or_default();

    if bits & U_MODEL != 0 { m.read_u8(); }
    if bits & U_FRAME != 0 { m.read_u8(); }
    if bits & U_COLORMAP != 0 { m.read_u8(); }
    if bits & U_SKIN != 0 { m.read_u8(); }
    if bits & U_EFFECTS != 0 { m.read_u8(); }

    let had_origin = bits & (U_ORIGIN1 | U_ORIGIN2 | U_ORIGIN3) != 0;
    if bits & U_ORIGIN1 != 0 { st.origin[0] = m.read_coord_q2(); }
    if bits & U_ANGLE1 != 0 { st.angles[0] = read_angle_i8(m); }
    if bits & U_ORIGIN2 != 0 { st.origin[1] = m.read_coord_q2(); }
    if bits & U_ANGLE2 != 0 { st.angles[1] = read_angle_i8(m); }
    if bits & U_ORIGIN3 != 0 { st.origin[2] = m.read_coord_q2(); }
    if bits & U_ANGLE3 != 0 { st.angles[2] = read_angle_i8(m); }

    let first = !st.seen;
    st.seen = true;
    ents.insert(num, st);

    if had_origin || first {
        let tick = (time * 1000.0) as i32;
        tb.pos(num as u32, tick, st.origin[0], st.origin[1], st.origin[2]);
        tb.yaw(num as u32, tick, st.angles[1], st.angles[0]);
    }
}

/// Baseline / static entity body: modelindex, frame, colormap, skin, then 3×
/// (coord, angle). Returns (origin, angles).
fn read_baseline_body(m: &mut ByteReader) -> ([f32; 3], [f32; 3]) {
    m.read_u8(); // modelindex
    m.read_u8(); // frame
    m.read_u8(); // colormap
    m.read_u8(); // skin
    let mut origin = [0.0f32; 3];
    let mut angles = [0.0f32; 3];
    for i in 0..3 {
        origin[i] = m.read_coord_q2();
        angles[i] = read_angle_i8(m);
    }
    (origin, angles)
}

/// CL_ParseClientdata: the local player's status block. Carries no entity
/// origin (that comes from the player entity's update) but must be consumed
/// exactly to stay aligned.
fn parse_clientdata(m: &mut ByteReader) {
    let bits = m.read_u16() as u32;
    if bits & SU_VIEWHEIGHT != 0 { m.read_i8(); }
    if bits & SU_IDEALPITCH != 0 { m.read_i8(); }
    for i in 0..3 {
        if bits & (SU_PUNCH1 << i) != 0 { m.read_i8(); }
        if bits & (SU_VELOCITY1 << i) != 0 { m.read_i8(); }
    }
    if bits & SU_ITEMS != 0 { m.read_i32(); }
    // SU_ONGROUND / SU_INWATER carry no data.
    if bits & SU_WEAPONFRAME != 0 { m.read_u8(); }
    if bits & SU_ARMOR != 0 { m.read_u8(); }
    if bits & SU_WEAPON != 0 { m.read_u8(); }
    m.read_i16(); // health
    m.read_u8(); // ammo
    m.read_u8(); // shells
    m.read_u8(); // nails
    m.read_u8(); // rockets
    m.read_u8(); // cells
    m.read_u8(); // active weapon
}

/// CL_ParseTEnt (NetQuake). Consume the variable layout to stay aligned.
fn parse_temp_entity(m: &mut ByteReader) {
    let te = m.read_u8();
    let n = match te {
        // point effects: pos (3 coords)
        0 /* SPIKE */ | 1 /* SUPERSPIKE */ | 2 /* GUNSHOT */ | 3 /* EXPLOSION */
        | 4 /* TAREXPLOSION */ | 7 /* WIZSPIKE */ | 8 /* KNIGHTSPIKE */
        | 10 /* LAVASPLASH */ | 11 /* TELEPORT */ => 6,
        // beams: entity(2) + start(6) + end(6)
        5 /* LIGHTNING1 */ | 6 /* LIGHTNING2 */ | 9 /* LIGHTNING3 */ | 13 /* BEAM */ => 14,
        // explosion2: pos(6) + colorStart(1) + colorLength(1)
        12 /* EXPLOSION2 */ => 8,
        _ => {
            m.overflow = true;
            0
        }
    };
    m.skip(n);
}

pub fn parse_qw_mvd(_data: &[u8]) -> Result<QuakeDemo, Box<dyn Error>> {
    Err("QuakeWorld .qwd/.mvd demos use an extension-negotiated protocol (the \
         sample carries FTE extensions) distinct from NetQuake; not yet supported. \
         Vanilla NetQuake .dem files decode via the Q1 path."
        .into())
}
