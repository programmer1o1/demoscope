// Quake 2 demo (.dm2, protocol 34) decoder.
//
// Container: a sequence of length-prefixed blocks - `i32 len` then `len` bytes
// of server message; `len == -1` marks EOF. Each block holds one or more svc_*
// messages. Positions live in two places, both delta-compressed:
//   - svc_packetentities: per-entity origin (1/8-unit shorts) + angles (bytes)
//   - svc_playerinfo:      the recorder's own pmove origin + viewangles
// Player names come from configstrings at CS_PLAYERSKINS + clientnum.
//
// Refs: Quake 2 3.20 source (qcommon/qcommon.h bit flags, client/cl_ents.c
// CL_ParseDelta / CL_ParsePacketEntities, client/cl_parse.c CL_ParseFrame).

use std::collections::HashMap;
use std::error::Error;

use super::{ByteReader, QuakeDemo, QuakeMeta, TrackBuilder};

// svc_ message ops (protocol 34)
const SVC_BAD: u8 = 0;
const SVC_MUZZLEFLASH: u8 = 1;
const SVC_MUZZLEFLASH2: u8 = 2;
const SVC_TEMP_ENTITY: u8 = 3;
const SVC_LAYOUT: u8 = 4;
const SVC_INVENTORY: u8 = 5;
const SVC_NOP: u8 = 6;
const SVC_DISCONNECT: u8 = 7;
const SVC_RECONNECT: u8 = 8;
const SVC_SOUND: u8 = 9;
const SVC_PRINT: u8 = 10;
const SVC_STUFFTEXT: u8 = 11;
const SVC_SERVERDATA: u8 = 12;
const SVC_CONFIGSTRING: u8 = 13;
const SVC_SPAWNBASELINE: u8 = 14;
const SVC_CENTERPRINT: u8 = 15;
const SVC_DOWNLOAD: u8 = 16;
const SVC_PLAYERINFO: u8 = 17;
const SVC_PACKETENTITIES: u8 = 18;
const SVC_DELTAPACKETENTITIES: u8 = 19;
const SVC_FRAME: u8 = 20;

// Entity-state delta bits (qcommon.h U_*)
const U_ORIGIN1: u32 = 1 << 0;
const U_ORIGIN2: u32 = 1 << 1;
const U_ANGLE2: u32 = 1 << 2;
const U_ANGLE3: u32 = 1 << 3;
const U_FRAME8: u32 = 1 << 4;
const U_EVENT: u32 = 1 << 5;
const U_REMOVE: u32 = 1 << 6;
const U_MOREBITS1: u32 = 1 << 7;
const U_NUMBER16: u32 = 1 << 8;
const U_ORIGIN3: u32 = 1 << 9;
const U_ANGLE1: u32 = 1 << 10;
const U_MODEL: u32 = 1 << 11;
const U_RENDERFX8: u32 = 1 << 12;
const U_EFFECTS8: u32 = 1 << 14;
const U_MOREBITS2: u32 = 1 << 15;
const U_SKIN8: u32 = 1 << 16;
const U_FRAME16: u32 = 1 << 17;
const U_RENDERFX16: u32 = 1 << 18;
const U_EFFECTS16: u32 = 1 << 19;
const U_MODEL2: u32 = 1 << 20;
const U_MODEL3: u32 = 1 << 21;
const U_MODEL4: u32 = 1 << 22;
const U_MOREBITS3: u32 = 1 << 23;
const U_OLDORIGIN: u32 = 1 << 24;
const U_SKIN16: u32 = 1 << 25;
const U_SOUND: u32 = 1 << 26;
const U_SOLID: u32 = 1 << 27;

// Player-state delta bits (PS_* from q_shared.h)
const PS_M_TYPE: u16 = 1 << 0;
const PS_M_ORIGIN: u16 = 1 << 1;
const PS_M_VELOCITY: u16 = 1 << 2;
const PS_M_TIME: u16 = 1 << 3;
const PS_M_FLAGS: u16 = 1 << 4;
const PS_M_GRAVITY: u16 = 1 << 5;
const PS_M_DELTA_ANGLES: u16 = 1 << 6;
const PS_VIEWOFFSET: u16 = 1 << 7;
const PS_VIEWANGLES: u16 = 1 << 8;
const PS_KICKANGLES: u16 = 1 << 9;
const PS_BLEND: u16 = 1 << 10;
const PS_FOV: u16 = 1 << 11;
const PS_WEAPONINDEX: u16 = 1 << 12;
const PS_WEAPONFRAME: u16 = 1 << 13;
const PS_RDFLAGS: u16 = 1 << 14;

// Configstring layout (protocol 34)
const MAX_MODELS: usize = 256;
const MAX_CLIENTS: usize = 256;
const CS_MODELS: usize = 32;
const CS_PLAYERSKINS: usize = CS_MODELS + MAX_MODELS * 5; // models,sounds,images,lights,items
const CS_NAME: usize = 0;

#[derive(Clone, Copy, Default)]
struct EntState {
    origin: [f32; 3],
    angles: [f32; 3],
    seen: bool,
}

pub fn parse(data: &[u8]) -> Result<QuakeDemo, Box<dyn Error>> {
    let dbg = std::env::var("DUMP_QUAKE").is_ok();
    let mut r = ByteReader::new(data);

    let mut tb = TrackBuilder::default();
    let mut configstrings: HashMap<usize, String> = HashMap::new();
    let mut ents: HashMap<u16, EntState> = HashMap::new();
    let mut map = String::new();
    let mut playernum: i32 = -1;
    let mut protocol = 34;
    let mut max_frame: i32 = 0;
    let mut nframes: usize = 0;
    let mut pm_type: i32 = 0; // recorder pmove type (PM_DEAD=2, PM_GIB=3), delta-persisted

    // Walk length-prefixed blocks.
    loop {
        if r.remaining() < 4 {
            break;
        }
        let block_len = r.read_i32();
        if block_len == -1 || block_len <= 0 {
            break; // EOF marker (or done)
        }
        let block = r.read_bytes(block_len as usize);
        if r.overflow {
            break;
        }
        let mut m = ByteReader::new(&block);

        // Parse all svc messages in this block.
        while !m.eof() && !m.overflow {
            let cmd = m.read_u8();
            match cmd {
                SVC_BAD => break,
                SVC_NOP => {}
                SVC_DISCONNECT | SVC_RECONNECT => {
                    break;
                }
                SVC_SERVERDATA => {
                    protocol = m.read_i32();
                    let _servercount = m.read_i32();
                    let _attractloop = m.read_u8();
                    let _gamedir = m.read_string();
                    playernum = m.read_i16() as i32;
                    map = m.read_string();
                    if dbg {
                        eprintln!("[q2] serverdata proto={protocol} playernum={playernum} map={map}");
                    }
                }
                SVC_CONFIGSTRING => {
                    let idx = m.read_u16() as usize;
                    let val = m.read_string();
                    configstrings.insert(idx, val);
                }
                SVC_SPAWNBASELINE => {
                    let (num, bits) = parse_entity_bits(&mut m);
                    let mut st = EntState::default();
                    parse_delta(&mut m, bits, &mut st);
                    st.seen = true;
                    if num != 0 {
                        ents.insert(num, st);
                    }
                }
                SVC_FRAME => {
                    let serverframe = m.read_i32();
                    let _deltaframe = m.read_i32();
                    let _surpress = m.read_u8(); // protocol 34 surpressCount
                    let areabytes = m.read_u8() as usize;
                    m.skip(areabytes);
                    max_frame = max_frame.max(serverframe);
                    nframes += 1;

                    // svc_playerinfo follows (the recorder's own state).
                    let c1 = m.read_u8();
                    if c1 == SVC_PLAYERINFO {
                        parse_playerinfo(&mut m, serverframe, playernum, &mut tb, &mut pm_type);
                    } else {
                        // Unexpected layout - bail on this block.
                        break;
                    }
                    // svc_packetentities follows.
                    let c2 = m.read_u8();
                    if c2 == SVC_PACKETENTITIES || c2 == SVC_DELTAPACKETENTITIES {
                        parse_packet_entities(&mut m, serverframe, &mut ents, &mut tb);
                    } else {
                        break;
                    }
                }
                // These can also appear standalone; handle defensively.
                SVC_PLAYERINFO => parse_playerinfo(&mut m, max_frame, playernum, &mut tb, &mut pm_type),
                SVC_PACKETENTITIES | SVC_DELTAPACKETENTITIES => {
                    parse_packet_entities(&mut m, max_frame, &mut ents, &mut tb)
                }
                SVC_MUZZLEFLASH | SVC_MUZZLEFLASH2 => {
                    // short(entity) + byte(flash id)
                    m.read_i16();
                    m.read_u8();
                }
                SVC_TEMP_ENTITY => parse_temp_entity(&mut m),
                SVC_LAYOUT => {
                    let _ = m.read_string();
                }
                SVC_INVENTORY => {
                    // MAX_ITEMS (256) shorts
                    m.skip(256 * 2);
                }
                SVC_PRINT => {
                    let _level = m.read_u8();
                    let _ = m.read_string();
                }
                SVC_STUFFTEXT | SVC_CENTERPRINT => {
                    let _ = m.read_string();
                }
                SVC_SOUND => {
                    // flags byte gates the optional fields; just resync by
                    // reading the common fixed part. Simplest safe handling:
                    // sounds don't carry positions we need, but their variable
                    // layout can desync the stream, so decode it properly.
                    let flags = m.read_u8();
                    let _soundnum = m.read_u8();
                    if flags & 0x01 != 0 { m.read_u8(); } // volume
                    if flags & 0x02 != 0 { m.read_u8(); } // attenuation
                    if flags & 0x10 != 0 { m.read_u8(); } // offset
                    if flags & 0x08 != 0 { m.read_i16(); } // entity+channel
                    if flags & 0x04 != 0 { m.skip(6); } // position (3 coords)
                }
                SVC_DOWNLOAD => {
                    let size = m.read_i16();
                    let _pct = m.read_u8();
                    if size > 0 {
                        m.skip(size as usize);
                    }
                }
                _ => {
                    // Unknown op: we can't know its length, so stop this block
                    // rather than desync. (Movement-bearing ops are all handled.)
                    if dbg {
                        eprintln!("[q2] unknown svc {cmd} @ block pos {}", m.pos);
                    }
                    break;
                }
            }
        }
    }

    // Names from configstrings: CS_PLAYERSKINS+clientnum = "name\skin".
    for clientnum in 0..MAX_CLIENTS {
        if let Some(s) = configstrings.get(&(CS_PLAYERSKINS + clientnum)) {
            let name = s.split('\\').next().unwrap_or("").to_string();
            if !name.is_empty() {
                // entity number for player = clientnum + 1
                tb.name((clientnum as u32) + 1, name);
            }
        }
    }
    if playernum >= 0 {
        tb.primary = Some((playernum as u32) + 1);
    }
    let server = configstrings.get(&CS_NAME).cloned().unwrap_or_default();

    let tick_rate = 10.0; // Q2 server runs at 10 Hz
    let duration = max_frame as f32 / tick_rate;
    let total_samples: usize = tb.tracks.values().map(|v| v.len()).sum();
    eprintln!(
        "  [Quake2] proto={protocol} map={} players={} entities={} samples={} frames={}",
        map,
        tb.names.len(),
        tb.tracks.len(),
        total_samples,
        nframes
    );

    let meta = QuakeMeta {
        map: map.clone(),
        server,
        client: tb
            .primary
            .and_then(|p| tb.names.get(&p).cloned())
            .unwrap_or_default(),
        game: "quake2".to_string(),
        protocol,
        duration,
        tick_rate,
        ncmds: nframes,
    };
    let mpd = tb.build(map, meta.server.clone(), duration, max_frame);
    Ok(QuakeDemo { meta, mpd })
}

/// ParseEntityBits: assemble the variable-width bitmask + entity number.
fn parse_entity_bits(m: &mut ByteReader) -> (u16, u32) {
    let mut bits = m.read_u8() as u32;
    if bits & U_MOREBITS1 != 0 {
        bits |= (m.read_u8() as u32) << 8;
    }
    if bits & U_MOREBITS2 != 0 {
        bits |= (m.read_u8() as u32) << 16;
    }
    if bits & U_MOREBITS3 != 0 {
        bits |= (m.read_u8() as u32) << 24;
    }
    let number = if bits & U_NUMBER16 != 0 {
        m.read_u16()
    } else {
        m.read_u8() as u16
    };
    (number, bits)
}

/// CL_ParseDelta: apply the present fields to an entity state in wire order.
fn parse_delta(m: &mut ByteReader, bits: u32, st: &mut EntState) {
    if bits & U_MODEL != 0 { m.read_u8(); }
    if bits & U_MODEL2 != 0 { m.read_u8(); }
    if bits & U_MODEL3 != 0 { m.read_u8(); }
    if bits & U_MODEL4 != 0 { m.read_u8(); }

    if bits & U_FRAME8 != 0 { m.read_u8(); }
    if bits & U_FRAME16 != 0 { m.read_i16(); }

    if (bits & U_SKIN8 != 0) && (bits & U_SKIN16 != 0) {
        m.read_i32();
    } else if bits & U_SKIN8 != 0 {
        m.read_u8();
    } else if bits & U_SKIN16 != 0 {
        m.read_i16();
    }

    if (bits & U_EFFECTS8 != 0) && (bits & U_EFFECTS16 != 0) {
        m.read_i32();
    } else if bits & U_EFFECTS8 != 0 {
        m.read_u8();
    } else if bits & U_EFFECTS16 != 0 {
        m.read_i16();
    }

    if (bits & U_RENDERFX8 != 0) && (bits & U_RENDERFX16 != 0) {
        m.read_i32();
    } else if bits & U_RENDERFX8 != 0 {
        m.read_u8();
    } else if bits & U_RENDERFX16 != 0 {
        m.read_i16();
    }

    if bits & U_ORIGIN1 != 0 { st.origin[0] = m.read_coord_q2(); }
    if bits & U_ORIGIN2 != 0 { st.origin[1] = m.read_coord_q2(); }
    if bits & U_ORIGIN3 != 0 { st.origin[2] = m.read_coord_q2(); }

    if bits & U_ANGLE1 != 0 { st.angles[0] = m.read_angle8(); }
    if bits & U_ANGLE2 != 0 { st.angles[1] = m.read_angle8(); }
    if bits & U_ANGLE3 != 0 { st.angles[2] = m.read_angle8(); }

    if bits & U_OLDORIGIN != 0 { m.skip(6); }
    if bits & U_SOUND != 0 { m.read_u8(); }
    if bits & U_EVENT != 0 { m.read_u8(); }
    if bits & U_SOLID != 0 { m.read_i16(); }
}

/// CL_ParseTEnt: temp entities carry no track data, but their layout is
/// variable so we must consume them exactly or the stream desyncs. Vanilla
/// Quake 2 3.20 TE table (q_shared.h). ReadPos = 6 bytes, ReadDir = 1 byte.
fn parse_temp_entity(m: &mut ByteReader) {
    let te = m.read_u8();
    let n = match te {
        // pos + dir
        0  /* GUNSHOT */ | 1 /* BLOOD */ | 2 /* BLASTER */ | 4 /* SHOTGUN */
        | 9 /* SPARKS */ | 12 /* SCREEN_SPARKS */ | 13 /* SHIELD_SPARKS */
        | 14 /* BULLET_SPARKS */ | 26 /* GREENBLOOD */ => 7,
        // pos only
        5 /* EXPLOSION1 */ | 6 /* EXPLOSION2 */ | 7 /* ROCKET_EXPLOSION */
        | 8 /* GRENADE_EXPLOSION */ | 17 /* ROCKET_EXPLOSION_WATER */
        | 18 /* GRENADE_EXPLOSION_WATER */ | 20 /* BFG_EXPLOSION */
        | 21 /* BFG_BIGEXPLOSION */ | 22 /* BOSSTPORT */
        | 28 /* PLASMA_EXPLOSION */ => 6,
        // pos + pos
        3 /* RAILTRAIL */ | 11 /* BUBBLETRAIL */ | 23 /* BFG_LASER */
        | 27 /* BLUEHYPERBLASTER */ => 12,
        // byte + pos + dir + byte
        10 /* SPLASH */ | 15 /* LASER_SPARKS */ | 25 /* WELDING_SPARKS */
        | 29 /* TUNNEL_SPARKS */ => 9,
        // short + pos + pos
        16 /* PARASITE_ATTACK */ | 19 /* MEDIC_CABLE_ATTACK */ => 14,
        // short + pos + pos + pos
        24 /* GRAPPLE_CABLE */ => 20,
        _ => {
            // Unknown TE (mod/expansion): we can't know its length. Latch
            // overflow so the block parse loop stops cleanly rather than
            // walking off into garbage.
            m.overflow = true;
            0
        }
    };
    m.skip(n);
}

/// CL_ParsePacketEntities: a run of entity deltas terminated by number == 0.
fn parse_packet_entities(
    m: &mut ByteReader,
    frame: i32,
    ents: &mut HashMap<u16, EntState>,
    tb: &mut TrackBuilder,
) {
    loop {
        if m.overflow {
            break;
        }
        let (number, bits) = parse_entity_bits(m);
        if number == 0 {
            break; // end of packetentities
        }
        if bits & U_REMOVE != 0 {
            ents.remove(&number);
            continue;
        }
        let mut st = ents.get(&number).copied().unwrap_or_default();
        let had_origin = bits & (U_ORIGIN1 | U_ORIGIN2 | U_ORIGIN3) != 0;
        parse_delta(m, bits, &mut st);
        let first = !st.seen;
        st.seen = true;
        ents.insert(number, st);

        // Sample on movement or first sight, so the path captures motion
        // without spamming a point per stationary entity per frame.
        if had_origin || first {
            tb.pos(number as u32, frame, st.origin[0], st.origin[1], st.origin[2]);
            tb.yaw(number as u32, frame, st.angles[1], st.angles[0]);
        }
    }
}

/// CL_ParsePlayerstate: the recorder's own pmove origin + viewangles.
fn parse_playerinfo(
    m: &mut ByteReader,
    frame: i32,
    playernum: i32,
    tb: &mut TrackBuilder,
    pm_type: &mut i32,
) {
    let flags = m.read_u16();

    if flags & PS_M_TYPE != 0 { *pm_type = m.read_u8() as i32; }

    let mut origin = None;
    if flags & PS_M_ORIGIN != 0 {
        // pmove origin is a 1/8-unit short.
        let x = m.read_i16() as f32 * 0.125;
        let y = m.read_i16() as f32 * 0.125;
        let z = m.read_i16() as f32 * 0.125;
        origin = Some([x, y, z]);
    }
    if flags & PS_M_VELOCITY != 0 { m.skip(6); }
    if flags & PS_M_TIME != 0 { m.read_u8(); }
    if flags & PS_M_FLAGS != 0 { m.read_u8(); }
    if flags & PS_M_GRAVITY != 0 { m.read_i16(); }
    if flags & PS_M_DELTA_ANGLES != 0 { m.skip(6); }

    if flags & PS_VIEWOFFSET != 0 { m.skip(3); }

    let mut viewangles = None;
    if flags & PS_VIEWANGLES != 0 {
        let pitch = m.read_angle16();
        let yaw = m.read_angle16();
        let _roll = m.read_angle16();
        viewangles = Some((pitch, yaw));
    }
    if flags & PS_KICKANGLES != 0 { m.skip(3); }

    if flags & PS_WEAPONINDEX != 0 { m.read_u8(); }
    if flags & PS_WEAPONFRAME != 0 { m.read_u8(); }
    if flags & PS_BLEND != 0 { m.skip(4); }
    if flags & PS_FOV != 0 { m.read_u8(); }
    if flags & PS_RDFLAGS != 0 { m.read_u8(); }

    // stats bitmask: i32 then a short per set bit.
    let statbits = m.read_i32() as u32;
    for i in 0..32 {
        if statbits & (1 << i) != 0 {
            m.read_i16();
        }
    }

    if playernum >= 0 {
        let eid = (playernum as u32) + 1;
        if let Some(o) = origin {
            tb.pos(eid, frame, o[0], o[1], o[2]);
        }
        if let Some((p, y)) = viewangles {
            tb.yaw(eid, frame, y, p);
            tb.view_angles.push((frame, p, y));
        }
        // Q2 pmtype: PM_DEAD = 2, PM_GIB = 3 → hide the recorder while dead.
        tb.life(eid, frame, *pm_type == 2 || *pm_type == 3);
    }
}
