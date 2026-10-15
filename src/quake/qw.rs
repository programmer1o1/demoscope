// QuakeWorld demo decoder: `.mvd` (multi-view server recordings) and `.qwd`
// (client recordings), protocol 24-28 plus the FTE and MVDSV extensions that
// change the wire layout.
//
// Both containers carry the same server message stream; only the framing
// differs:
//   .mvd  per frame: u8 msec-since-last-frame, u8 cmd (low 3 bits = type,
//         high 5 = target player). dem_multiple adds a u32 player mask; every
//         message-bearing type then has a u32 length + payload.
//   .qwd  per frame: f32 absolute time, u8 cmd. dem_cmd = 24-byte usercmd +
//         3 f32 view angles; dem_read = u32 length + netchan packet (8-byte
//         sequence header, or a -1 connectionless marker).
//
// Players are not in packetentities in QuakeWorld: each has its own
// svc_playerinfo. MVD playerinfo is delta-coded per player (DF_* flags);
// QWD playerinfo is the normal client form (PF_* flags). Entity updates are
// consumed but not tracked, since only players are drawn.
//
// Layouts follow the QuakeWorld client and server sources (ezQuake / MVDSV,
// shared `protocol.h`): CL_GetDemoMessage, CL_ParseServerMessage,
// CL_ParsePlayerinfo, CL_ParsePacketEntities, CL_ParseDelta, CL_ParseTEnt.

use std::collections::HashMap;
use std::error::Error;

use super::{ByteReader, QuakeDemo, QuakeMeta, TrackBuilder};

// Demo frame types.
const DEM_CMD: u8 = 0;
const DEM_READ: u8 = 1;
const DEM_SET: u8 = 2;
const DEM_MULTIPLE: u8 = 3;
const DEM_SINGLE: u8 = 4;
const DEM_STATS: u8 = 5;
const DEM_ALL: u8 = 6;

// Protocol version markers in svc_serverdata.
const PROTOCOL_VERSION_FTE: i32 = i32::from_le_bytes(*b"FTEX");
const PROTOCOL_VERSION_FTE2: i32 = i32::from_le_bytes(*b"FTE2");
const PROTOCOL_VERSION_MVD1: i32 = i32::from_le_bytes(*b"MVD1");

// FTE extension bits that change the wire format.
const FTE_PEXT_TRANS: u32 = 0x0000_0008;
const FTE_PEXT_ENTITYDBL: u32 = 0x0000_2000;
const FTE_PEXT_FLOATCOORDS: u32 = 0x0000_8000;
const FTE_PEXT_COLOURMOD: u32 = 0x0008_0000;
const FTE_PEXT_SPAWNSTATIC2: u32 = 0x0040_0000;

// MVDSV extension bits.
const MVD_PEXT1_FLOATCOORDS: u32 = 1 << 0;
const MVD_PEXT1_HIGHLAGTELEPORT: u32 = 1 << 1;
const MVD_PEXT1_WEAPONPREDICTION: u32 = 1 << 7;

// Server messages.
const SVC_BAD: u8 = 0;
const SVC_NOP: u8 = 1;
const SVC_DISCONNECT: u8 = 2;
const SVC_UPDATESTAT: u8 = 3;
const SVC_NQ_TIME: u8 = 7;
const SVC_PRINT: u8 = 8;
const SVC_STUFFTEXT: u8 = 9;
const SVC_SETANGLE: u8 = 10;
const SVC_SERVERDATA: u8 = 11;
const SVC_LIGHTSTYLE: u8 = 12;
const SVC_UPDATEFRAGS: u8 = 14;
const SVC_STOPSOUND: u8 = 16;
const SVC_SOUND: u8 = 6;
const SVC_DAMAGE: u8 = 19;
const SVC_SPAWNSTATIC: u8 = 20;
const SVC_FTE_SPAWNSTATIC2: u8 = 21;
const SVC_SPAWNBASELINE: u8 = 22;
const SVC_TEMP_ENTITY: u8 = 23;
const SVC_SETPAUSE: u8 = 24;
const SVC_CENTERPRINT: u8 = 26;
const SVC_KILLEDMONSTER: u8 = 27;
const SVC_FOUNDSECRET: u8 = 28;
const SVC_SPAWNSTATICSOUND: u8 = 29;
const SVC_INTERMISSION: u8 = 30;
const SVC_FINALE: u8 = 31;
const SVC_CDTRACK: u8 = 32;
const SVC_SELLSCREEN: u8 = 33;
const SVC_SMALLKICK: u8 = 34;
const SVC_BIGKICK: u8 = 35;
const SVC_UPDATEPING: u8 = 36;
const SVC_UPDATEENTERTIME: u8 = 37;
const SVC_UPDATESTATLONG: u8 = 38;
const SVC_MUZZLEFLASH: u8 = 39;
const SVC_UPDATEUSERINFO: u8 = 40;
const SVC_DOWNLOAD: u8 = 41;
const SVC_PLAYERINFO: u8 = 42;
const SVC_NAILS: u8 = 43;
const SVC_CHOKECOUNT: u8 = 44;
const SVC_MODELLIST: u8 = 45;
const SVC_SOUNDLIST: u8 = 46;
const SVC_PACKETENTITIES: u8 = 47;
const SVC_DELTAPACKETENTITIES: u8 = 48;
const SVC_MAXSPEED: u8 = 49;
const SVC_ENTGRAVITY: u8 = 50;
const SVC_SETINFO: u8 = 51;
const SVC_SERVERINFO: u8 = 52;
const SVC_UPDATEPL: u8 = 53;
const SVC_NAILS2: u8 = 54;
const SVC_FTE_MODELLISTSHORT: u8 = 60;
const SVC_FTE_SPAWNBASELINE2: u8 = 66;
const SVC_QIZMOVOICE: u8 = 83;

// Entity delta bits (the u16 word holds the entity number in its low 9 bits).
const U_ORIGIN1: u32 = 1 << 9;
const U_ORIGIN2: u32 = 1 << 10;
const U_ORIGIN3: u32 = 1 << 11;
const U_ANGLE2: u32 = 1 << 12;
const U_FRAME: u32 = 1 << 13;
const U_REMOVE: u32 = 1 << 14;
const U_MOREBITS: u32 = 1 << 15;
const U_ANGLE1: u32 = 1 << 0;
const U_ANGLE3: u32 = 1 << 1;
const U_MODEL: u32 = 1 << 2;
const U_COLORMAP: u32 = 1 << 3;
const U_SKIN: u32 = 1 << 4;
const U_EFFECTS: u32 = 1 << 5;
const U_FTE_EVENMORE: u32 = 1 << 7;
const U_FTE_TRANS: u32 = 1 << 1;
const U_FTE_MODELDBL: u32 = 1 << 3;
const U_FTE_YETMORE: u32 = 1 << 7;
const U_FTE_COLOURMOD: u32 = 1 << 10;

// Client playerinfo flags (.qwd).
const PF_MSEC: u32 = 1 << 0;
const PF_COMMAND: u32 = 1 << 1;
const PF_VELOCITY1: u32 = 1 << 2;
const PF_MODEL: u32 = 1 << 5;
const PF_SKINNUM: u32 = 1 << 6;
const PF_EFFECTS: u32 = 1 << 7;
const PF_WEAPONFRAME: u32 = 1 << 8;
const PF_DEAD: u32 = 1 << 9;
const PF_EXTRA_PFS: u32 = 1 << 15;
const PF_TRANS_Z: u32 = 1 << 17;

// MVD playerinfo flags.
const DF_ORIGIN: u32 = 1;
const DF_ANGLES: u32 = 1 << 3;
const DF_EFFECTS: u32 = 1 << 6;
const DF_SKINNUM: u32 = 1 << 7;
const DF_DEAD: u32 = 1 << 8;
const DF_WEAPONFRAME: u32 = 1 << 10;
const DF_MODEL: u32 = 1 << 11;

// Usercmd bits.
const CM_ANGLE1: u8 = 1 << 0;
const CM_ANGLE3: u8 = 1 << 1;
const CM_FORWARD: u8 = 1 << 2;
const CM_SIDE: u8 = 1 << 3;
const CM_UP: u8 = 1 << 4;
const CM_BUTTONS: u8 = 1 << 5;
const CM_IMPULSE: u8 = 1 << 6;
const CM_ANGLE2: u8 = 1 << 7;

const MAX_CLIENTS: u8 = 32;
const QWD_USERCMD_SIZE: usize = 24;

#[derive(Clone, Copy, Default)]
struct Player {
    origin: [f32; 3],
    angles: [f32; 3],
    seen: bool,
}

struct Ctx {
    mvd: bool,
    fte: u32,
    mvd1: u32,
    protover: i32,
    float_coords: bool,
    serverdata_seen: bool,
    done: bool,
    tick: i32,
    playernum: Option<u8>,
    map: String,
    level_name: String,
    server_name: String,
    players: [Player; MAX_CLIENTS as usize],
    names: HashMap<u8, String>,
    frags: HashMap<u8, i16>,
    tb: TrackBuilder,
}

impl Ctx {
    fn new(mvd: bool) -> Self {
        Ctx {
            mvd,
            fte: 0,
            mvd1: 0,
            protover: 28,
            float_coords: false,
            serverdata_seen: false,
            done: false,
            tick: 0,
            playernum: None,
            map: String::new(),
            level_name: String::new(),
            server_name: String::new(),
            players: [Player::default(); MAX_CLIENTS as usize],
            names: HashMap::new(),
            frags: HashMap::new(),
            tb: TrackBuilder::default(),
        }
    }

    /// MSG_ReadCoord: a float with FTE_PEXT_FLOATCOORDS, else a 1/8-unit short.
    fn coord(&self, m: &mut ByteReader) -> f32 {
        if self.float_coords {
            m.read_f32()
        } else {
            m.read_coord_q2()
        }
    }

    /// Entity and player origins use plain floats under MVD_PEXT1_FLOATCOORDS.
    fn ent_coord(&self, m: &mut ByteReader) -> f32 {
        if self.mvd1 & MVD_PEXT1_FLOATCOORDS != 0 {
            m.read_f32()
        } else {
            self.coord(m)
        }
    }

    /// MSG_ReadAngle: 16-bit with FTE_PEXT_FLOATCOORDS, else 8-bit signed.
    fn angle(&self, m: &mut ByteReader) {
        if self.float_coords {
            m.skip(2);
        } else {
            m.skip(1);
        }
    }

    fn skip_coords(&self, m: &mut ByteReader, n: usize) {
        for _ in 0..n {
            self.coord(m);
        }
    }
}

pub fn parse(data: &[u8], name: &str) -> Result<QuakeDemo, Box<dyn Error>> {
    decode(data, name).map(|(demo, _)| demo)
}

/// Decodes the demo and also returns how many messages failed to parse.
fn decode(data: &[u8], name: &str) -> Result<(QuakeDemo, usize), Box<dyn Error>> {
    let mvd = name.to_lowercase().ends_with(".mvd");
    let dbg = std::env::var("DUMP_QUAKE").is_ok();
    let mut ctx = Ctx::new(mvd);
    let mut r = ByteReader::new(data);
    let mut frames = 0usize;
    let mut bad_messages = 0usize;

    while !ctx.done && !r.eof() {
        let start = r.pos;
        let payload: Option<&[u8]> = if mvd {
            match read_mvd_frame(&mut r, &mut ctx) {
                Ok(p) => p,
                Err(e) => {
                    if dbg {
                        eprintln!("[qw] frame error @ {start}: {e}");
                    }
                    break;
                }
            }
        } else {
            match read_qwd_frame(&mut r, &mut ctx) {
                Ok(p) => p,
                Err(e) => {
                    if dbg {
                        eprintln!("[qw] frame error @ {start}: {e}");
                    }
                    break;
                }
            }
        };
        if r.overflow {
            break;
        }
        frames += 1;
        if let Some(bytes) = payload {
            let mut m = ByteReader::new(bytes);
            if let Err(e) = parse_message(&mut m, &mut ctx) {
                bad_messages += 1;
                if dbg {
                    eprintln!("[qw] message error in frame @ {start}: {e}");
                }
            }
        }
    }

    if dbg {
        eprintln!(
            "[qw] stopped at byte {} of {} (disconnect={})",
            r.pos,
            data.len(),
            ctx.done
        );
        let mut slots: Vec<_> = ctx.frags.iter().collect();
        slots.sort();
        for (slot, frags) in slots {
            let deaths = ctx
                .tb
                .life_states
                .get(&(*slot as u32 + 1))
                .map_or(0, |v| v.iter().filter(|(_, s)| *s != 0).count());
            eprintln!(
                "[qw] slot {slot} {:?}: frags {frags}, deaths {deaths}",
                ctx.names.get(slot)
            );
        }
    }
    if !ctx.serverdata_seen {
        return Err("not a QuakeWorld demo: no svc_serverdata found".into());
    }

    for (slot, name) in std::mem::take(&mut ctx.names) {
        ctx.tb.name(slot as u32 + 1, name);
    }
    if let Some(p) = ctx.playernum {
        if ctx.tb.tracks.contains_key(&(p as u32 + 1)) {
            ctx.tb.primary = Some(p as u32 + 1);
        }
    }
    // An MVD has no recorder, so no camera-angle stream. The viewer defaults to
    // the lowest slot as its primary, so use that player's aim as the dense
    // first-person camera path instead of stepping through the thinned yaws.
    if ctx.tb.view_angles.is_empty() {
        let primary = ctx
            .tb
            .primary
            .or_else(|| ctx.tb.tracks.keys().min().copied());
        if let Some(yaws) = primary.and_then(|p| ctx.tb.yaws.get(&p)) {
            ctx.tb.view_angles = yaws
                .iter()
                .map(|&(t, yaw, pitch)| (t, pitch, yaw))
                .collect();
        }
    }

    let tick_rate = 1000.0;
    let duration = ctx.tick as f32 / tick_rate;
    let total_samples: usize = ctx.tb.tracks.values().map(|v| v.len()).sum();
    eprintln!(
        "  [QuakeWorld/{}] map={} protocol={} fte=0x{:x} mvd1=0x{:x} players={} tracks={} samples={} frames={} bad_messages={} duration={:.1}s",
        if mvd { "MVD" } else { "QWD" },
        ctx.map,
        ctx.protover,
        ctx.fte,
        ctx.mvd1,
        ctx.tb.names.len(),
        ctx.tb.tracks.len(),
        total_samples,
        frames,
        bad_messages,
        duration
    );

    let client = ctx
        .tb
        .primary
        .and_then(|p| ctx.tb.names.get(&p).cloned())
        .unwrap_or_default();
    let server = if ctx.server_name.is_empty() {
        ctx.level_name.clone()
    } else {
        ctx.server_name.clone()
    };
    let meta = QuakeMeta {
        map: ctx.map.clone(),
        server: server.clone(),
        client,
        game: "quakeworld".to_string(),
        protocol: ctx.protover,
        duration,
        tick_rate,
        ncmds: frames,
    };
    let mpd = ctx.tb.build(ctx.map, server, duration, ctx.tick);
    Ok((QuakeDemo { meta, mpd }, bad_messages))
}

/// One MVD frame. Returns the message payload to parse, or None for frames
/// that carry no server messages (dem_set, and dem_multiple sent to nobody,
/// which MVDSV uses for hidden side-channel data).
fn read_mvd_frame<'a>(r: &mut ByteReader<'a>, ctx: &mut Ctx) -> Result<Option<&'a [u8]>, String> {
    let msec = r.read_u8();
    ctx.tick += msec as i32;
    let cmd = r.read_u8();
    match cmd & 7 {
        DEM_SET => {
            r.skip(8);
            Ok(None)
        }
        DEM_MULTIPLE => {
            let mask = r.read_u32();
            let payload = read_payload(r)?;
            Ok(if mask == 0 { None } else { Some(payload) })
        }
        DEM_READ | DEM_SINGLE | DEM_STATS | DEM_ALL => Ok(Some(read_payload(r)?)),
        DEM_CMD => Err("dem_cmd in an MVD".into()),
        t => Err(format!("unknown frame type {t}")),
    }
}

/// One QWD frame. dem_cmd frames carry the recorder's view angles.
fn read_qwd_frame<'a>(r: &mut ByteReader<'a>, ctx: &mut Ctx) -> Result<Option<&'a [u8]>, String> {
    let time = r.read_f32();
    if time.is_finite() && time >= 0.0 {
        ctx.tick = ctx.tick.max((time * 1000.0) as i32);
    }
    let cmd = r.read_u8();
    match cmd {
        DEM_CMD => {
            r.skip(QWD_USERCMD_SIZE);
            let pitch = r.read_f32();
            let yaw = r.read_f32();
            let _roll = r.read_f32();
            ctx.tb.view_angles.push((ctx.tick, pitch, yaw));
            Ok(None)
        }
        DEM_READ => {
            let packet = read_payload(r)?;
            if packet.len() >= 4 && packet[..4] == [0xff; 4] {
                return Ok(None); // connectionless packet
            }
            // Skip the netchan sequence / acknowledge header.
            Ok(packet.get(8..))
        }
        DEM_SET => {
            r.skip(8);
            Ok(None)
        }
        t => Err(format!("unknown frame type {t}")),
    }
}

fn read_payload<'a>(r: &mut ByteReader<'a>) -> Result<&'a [u8], String> {
    let len = r.read_u32() as usize;
    if len > r.remaining() {
        return Err(format!("payload length {len} past end of file"));
    }
    let start = r.pos;
    r.skip(len);
    Ok(&r.data[start..start + len])
}

fn parse_message(m: &mut ByteReader, ctx: &mut Ctx) -> Result<(), String> {
    while !m.eof() {
        let cmd = m.read_u8();
        match cmd {
            SVC_BAD => return Err("svc_bad".into()),
            SVC_NOP | SVC_KILLEDMONSTER | SVC_FOUNDSECRET | SVC_SELLSCREEN | SVC_SMALLKICK
            | SVC_BIGKICK => {}
            SVC_DISCONNECT => {
                if !m.eof() {
                    m.read_string();
                }
                ctx.done = true;
                return Ok(());
            }
            SVC_UPDATESTAT => m.skip(2),
            SVC_NQ_TIME => m.skip(4),
            SVC_PRINT => {
                m.read_u8();
                m.read_string();
            }
            SVC_STUFFTEXT | SVC_CENTERPRINT | SVC_FINALE => {
                m.read_string();
            }
            SVC_SETANGLE => {
                if ctx.mvd || ctx.mvd1 & MVD_PEXT1_HIGHLAGTELEPORT != 0 {
                    m.read_u8();
                }
                for _ in 0..3 {
                    ctx.angle(m);
                }
            }
            SVC_SERVERDATA => {
                if ctx.serverdata_seen {
                    // A second map in the same file: keep the first map only.
                    ctx.done = true;
                    return Ok(());
                }
                parse_serverdata(m, ctx)?;
            }
            SVC_LIGHTSTYLE => {
                m.read_u8();
                m.read_string();
            }
            SVC_SOUND => {
                let channel = m.read_u16();
                if channel & (1 << 15) != 0 {
                    m.read_u8(); // volume
                }
                if channel & (1 << 14) != 0 {
                    m.read_u8(); // attenuation
                }
                m.read_u8(); // sound number
                ctx.skip_coords(m, 3);
            }
            SVC_UPDATEFRAGS => {
                let slot = m.read_u8();
                let frags = m.read_i16();
                ctx.frags.insert(slot, frags);
            }
            SVC_UPDATEPING => m.skip(3),
            SVC_STOPSOUND | SVC_MUZZLEFLASH => m.skip(2),
            SVC_DAMAGE => {
                m.skip(2);
                ctx.skip_coords(m, 3);
            }
            SVC_SPAWNSTATIC => skip_baseline(m, ctx),
            SVC_SPAWNBASELINE => {
                m.skip(2);
                skip_baseline(m, ctx);
            }
            SVC_FTE_SPAWNSTATIC2 | SVC_FTE_SPAWNBASELINE2 => {
                if ctx.fte & FTE_PEXT_SPAWNSTATIC2 == 0 {
                    return Err(format!("svc {cmd} without FTE_PEXT_SPAWNSTATIC2"));
                }
                let word = m.read_u16() as u32;
                skip_delta(m, ctx, word);
            }
            SVC_TEMP_ENTITY => parse_temp_entity(m, ctx)?,
            SVC_SETPAUSE | SVC_CDTRACK | SVC_CHOKECOUNT => m.skip(1),
            SVC_SPAWNSTATICSOUND => {
                ctx.skip_coords(m, 3);
                m.skip(3);
            }
            SVC_INTERMISSION => {
                ctx.skip_coords(m, 3);
                for _ in 0..3 {
                    ctx.angle(m);
                }
            }
            SVC_UPDATEENTERTIME => m.skip(5),
            SVC_UPDATESTATLONG => m.skip(5),
            SVC_UPDATEUSERINFO => {
                let slot = m.read_u8();
                m.read_i32(); // user id
                let info = m.read_string();
                update_userinfo(ctx, slot, &info);
            }
            SVC_SETINFO => {
                let slot = m.read_u8();
                let key = m.read_string();
                let value = m.read_string();
                if key == "name" && slot < MAX_CLIENTS && !value.is_empty() {
                    ctx.names.insert(slot, value);
                }
            }
            SVC_SERVERINFO => {
                let key = m.read_string();
                let value = m.read_string();
                apply_serverinfo(ctx, &key, &value);
            }
            SVC_DOWNLOAD => {
                let size = m.read_i16();
                m.read_u8(); // percent
                if size > 0 {
                    m.skip(size as usize);
                }
            }
            SVC_PLAYERINFO => {
                if ctx.mvd {
                    parse_playerinfo_mvd(m, ctx)?;
                } else {
                    parse_playerinfo_qwd(m, ctx)?;
                }
            }
            SVC_NAILS => {
                let n = m.read_u8() as usize;
                m.skip(n * 6);
            }
            SVC_NAILS2 => {
                let n = m.read_u8() as usize;
                m.skip(n * 7);
            }
            SVC_MODELLIST => parse_precache_list(m, ctx, false, true),
            SVC_FTE_MODELLISTSHORT => parse_precache_list(m, ctx, true, true),
            SVC_SOUNDLIST => parse_precache_list(m, ctx, false, false),
            SVC_PACKETENTITIES => skip_packet_entities(m, ctx, false)?,
            SVC_DELTAPACKETENTITIES => skip_packet_entities(m, ctx, true)?,
            SVC_MAXSPEED | SVC_ENTGRAVITY => m.skip(4),
            SVC_UPDATEPL => m.skip(2),
            SVC_QIZMOVOICE => m.skip(34),
            _ => return Err(format!("unknown svc {cmd} at {}", m.pos - 1)),
        }
        if m.overflow {
            return Err(format!("read past end after svc {cmd}"));
        }
    }
    Ok(())
}

fn parse_serverdata(m: &mut ByteReader, ctx: &mut Ctx) -> Result<(), String> {
    let protover = loop {
        let p = m.read_i32();
        if m.overflow {
            return Err("truncated svc_serverdata".into());
        }
        match p {
            PROTOCOL_VERSION_FTE => ctx.fte = m.read_u32(),
            PROTOCOL_VERSION_FTE2 => {
                m.read_u32();
            }
            PROTOCOL_VERSION_MVD1 => ctx.mvd1 = m.read_u32(),
            _ => break p,
        }
    };
    if !(24..=28).contains(&protover) {
        return Err(format!("unsupported QuakeWorld protocol {protover}"));
    }
    ctx.protover = protover;
    ctx.float_coords = ctx.fte & FTE_PEXT_FLOATCOORDS != 0;
    ctx.serverdata_seen = true;

    m.read_i32(); // server count
    m.read_string(); // game directory
    if ctx.mvd {
        m.read_f32(); // demo start time
    } else {
        ctx.playernum = Some(m.read_u8() & 0x7f); // high bit = spectator
    }
    ctx.level_name = m.read_string();
    if protover >= 25 {
        m.skip(10 * 4); // movevars
    }
    Ok(())
}

/// Model / sound precache list. The first model is the world, `maps/<name>.bsp`.
fn parse_precache_list(m: &mut ByteReader, ctx: &mut Ctx, short_start: bool, models: bool) {
    let mut index = if ctx.protover >= 26 {
        if short_start {
            m.read_u16() as u32
        } else {
            m.read_u8() as u32
        }
    } else {
        0
    };
    loop {
        let s = m.read_string();
        if s.is_empty() || m.overflow {
            break;
        }
        index += 1;
        if models && index == 1 && ctx.map.is_empty() {
            if let Some(stem) = s.strip_prefix("maps/").and_then(|p| p.strip_suffix(".bsp")) {
                ctx.map = stem.to_string();
            }
        }
    }
    if ctx.protover >= 26 {
        m.read_u8(); // next index (0 = complete)
    }
}

fn apply_serverinfo(ctx: &mut Ctx, key: &str, value: &str) {
    match key {
        "hostname" => ctx.server_name = value.to_string(),
        "map" if ctx.map.is_empty() => ctx.map = value.to_string(),
        _ => {}
    }
}

/// `\key\value\key\value...` userinfo: names and spectator status.
fn update_userinfo(ctx: &mut Ctx, slot: u8, info: &str) {
    if slot >= MAX_CLIENTS {
        return;
    }
    let mut name = String::new();
    let mut spectator = false;
    let mut parts = info.trim_start_matches('\\').split('\\');
    while let (Some(k), Some(v)) = (parts.next(), parts.next()) {
        match k {
            "name" => name = v.to_string(),
            "*spectator" => spectator = v != "0" && !v.is_empty(),
            _ => {}
        }
    }
    let eid = slot as u32 + 1;
    if name.is_empty() {
        // Slot emptied: the player left.
        if ctx.players[slot as usize].seen {
            ctx.tb.observe(eid, ctx.tick, true);
        }
        return;
    }
    ctx.names.insert(slot, name);
    if ctx.players[slot as usize].seen || spectator {
        ctx.tb.observe(eid, ctx.tick, spectator);
    }
}

/// MVD playerinfo: per-player delta against the last state sent for that slot.
fn parse_playerinfo_mvd(m: &mut ByteReader, ctx: &mut Ctx) -> Result<(), String> {
    let num = m.read_u8();
    if num >= MAX_CLIENTS {
        return Err(format!("playerinfo for slot {num}"));
    }
    let flags = m.read_u16() as u32;
    m.read_u8(); // frame
    let mut p = ctx.players[num as usize];
    let mut moved = false;
    for i in 0..3 {
        if flags & (DF_ORIGIN << i) != 0 {
            p.origin[i] = ctx.coord(m);
            moved = true;
        }
    }
    let mut turned = false;
    for i in 0..3 {
        if flags & (DF_ANGLES << i) != 0 {
            p.angles[i] = m.read_angle16();
            turned = true;
        }
    }
    for bit in [DF_MODEL, DF_SKINNUM, DF_EFFECTS, DF_WEAPONFRAME] {
        if flags & bit != 0 {
            m.read_u8();
        }
    }
    record_player(ctx, num, p, moved, turned, flags & DF_DEAD != 0);
    Ok(())
}

/// QWD playerinfo: the normal client form, full origin every time.
fn parse_playerinfo_qwd(m: &mut ByteReader, ctx: &mut Ctx) -> Result<(), String> {
    let num = m.read_u8();
    if num >= MAX_CLIENTS {
        return Err(format!("playerinfo for slot {num}"));
    }
    let mut flags = m.read_u16() as u32;
    if ctx.fte & FTE_PEXT_TRANS != 0 && flags & PF_EXTRA_PFS != 0 {
        flags |= (m.read_u8() as u32) << 16;
    }
    let mut p = ctx.players[num as usize];
    for i in 0..3 {
        p.origin[i] = ctx.ent_coord(m);
    }
    m.read_u8(); // frame
    if flags & PF_MSEC != 0 {
        m.read_u8();
    }
    let mut turned = false;
    if flags & PF_COMMAND != 0 {
        let angles = read_delta_usercmd(m, ctx.protover);
        p.angles = angles;
        turned = true;
    }
    for i in 0..3 {
        if flags & (PF_VELOCITY1 << i) != 0 {
            m.read_i16();
        }
    }
    for bit in [PF_MODEL, PF_SKINNUM, PF_EFFECTS] {
        if flags & bit != 0 {
            m.read_u8();
        }
    }
    if flags & PF_WEAPONFRAME != 0 {
        m.read_u8();
        if ctx.mvd1 & MVD_PEXT1_WEAPONPREDICTION != 0 && m.read_u8() != 0 {
            // impulse, weapon, 3 timing floats, 3 flag bytes, 4 ammo bytes
            m.skip(1 + 2 + 12 + 3 + 4);
        }
    }
    if flags & PF_TRANS_Z != 0 && ctx.fte & FTE_PEXT_TRANS != 0 {
        m.read_u8();
    }
    record_player(ctx, num, p, true, turned, flags & PF_DEAD != 0);
    Ok(())
}

fn record_player(ctx: &mut Ctx, num: u8, mut p: Player, moved: bool, turned: bool, dead: bool) {
    let eid = num as u32 + 1;
    let first = !p.seen;
    p.seen = true;
    ctx.players[num as usize] = p;
    if moved || first {
        ctx.tb
            .pos(eid, ctx.tick, p.origin[0], p.origin[1], p.origin[2]);
    }
    if turned || first {
        ctx.tb.yaw(eid, ctx.tick, p.angles[1], p.angles[0]);
    }
    if first {
        ctx.tb.observe(eid, ctx.tick, false);
    }
    ctx.tb.life(eid, ctx.tick, dead);
}

/// MSG_ReadDeltaUsercmd against a null command. Returns the view angles.
fn read_delta_usercmd(m: &mut ByteReader, protover: i32) -> [f32; 3] {
    let bits = m.read_u8();
    let mut angles = [0.0f32; 3];
    if protover <= 26 {
        if bits & CM_ANGLE1 != 0 {
            angles[0] = m.read_angle16();
        }
        angles[1] = m.read_angle16();
        if bits & CM_ANGLE3 != 0 {
            angles[2] = m.read_angle16();
        }
        for bit in [CM_FORWARD, CM_SIDE, CM_UP] {
            if bits & bit != 0 {
                m.read_u8();
            }
        }
    } else {
        for (i, bit) in [CM_ANGLE1, CM_ANGLE2, CM_ANGLE3].into_iter().enumerate() {
            if bits & bit != 0 {
                angles[i] = m.read_angle16();
            }
        }
        for bit in [CM_FORWARD, CM_SIDE, CM_UP] {
            if bits & bit != 0 {
                m.read_i16();
            }
        }
    }
    if bits & CM_BUTTONS != 0 {
        m.read_u8();
    }
    if bits & CM_IMPULSE != 0 {
        m.read_u8();
    }
    // msec: optional (bit 7) before protocol 27, always sent from 27 on.
    if protover >= 27 || bits & CM_ANGLE2 != 0 {
        m.read_u8();
    }
    angles
}

/// Baseline / static entity: model, frame, colormap, skin, then 3 x (coord, angle).
fn skip_baseline(m: &mut ByteReader, ctx: &Ctx) {
    m.skip(4);
    for _ in 0..3 {
        ctx.coord(m);
        ctx.angle(m);
    }
}

/// CL_ParseDelta's byte layout. `word` is the already-read u16 header.
fn skip_delta(m: &mut ByteReader, ctx: &Ctx, word: u32) {
    let mut bits = word & !511;
    if bits & U_MOREBITS != 0 {
        bits |= m.read_u8() as u32;
    }
    let mut more = 0u32;
    if bits & U_FTE_EVENMORE != 0 && ctx.fte != 0 {
        more = m.read_u8() as u32;
        if more & U_FTE_YETMORE != 0 {
            more |= (m.read_u8() as u32) << 8;
        }
    }
    if bits & U_MODEL != 0 {
        m.read_u8();
    } else if more & U_FTE_MODELDBL != 0 {
        m.read_u16();
    }
    for bit in [U_FRAME, U_COLORMAP, U_SKIN, U_EFFECTS] {
        if bits & bit != 0 {
            m.read_u8();
        }
    }
    for (origin, angle) in [
        (U_ORIGIN1, U_ANGLE1),
        (U_ORIGIN2, U_ANGLE2),
        (U_ORIGIN3, U_ANGLE3),
    ] {
        if bits & origin != 0 {
            ctx.ent_coord(m);
        }
        if bits & angle != 0 {
            ctx.angle(m);
        }
    }
    if more & U_FTE_TRANS != 0 && ctx.fte & FTE_PEXT_TRANS != 0 {
        m.read_u8();
    }
    if more & U_FTE_COLOURMOD != 0 && ctx.fte & FTE_PEXT_COLOURMOD != 0 {
        m.skip(3);
    }
}

fn skip_packet_entities(m: &mut ByteReader, ctx: &Ctx, delta: bool) -> Result<(), String> {
    if delta {
        m.read_u8(); // delta-from sequence
    }
    loop {
        let word = m.read_u16() as u32;
        if m.overflow {
            return Err("truncated packetentities".into());
        }
        if word == 0 {
            return Ok(());
        }
        if word & U_REMOVE != 0 {
            if word & U_MOREBITS != 0
                && ctx.fte & FTE_PEXT_ENTITYDBL != 0
                && m.read_u8() as u32 & U_FTE_EVENMORE != 0
            {
                m.read_u8();
            }
            continue;
        }
        skip_delta(m, ctx, word);
    }
}

/// QuakeWorld temp entities (CL_ParseTEnt). Differs from NetQuake: gunshot
/// and blood carry a count byte.
fn parse_temp_entity(m: &mut ByteReader, ctx: &Ctx) -> Result<(), String> {
    let te = m.read_u8();
    match te {
        // lightning beams: entity + start + end
        5 | 6 | 9 => {
            m.skip(2);
            ctx.skip_coords(m, 6);
        }
        // gunshot / blood: count + position
        2 | 12 => {
            m.skip(1);
            ctx.skip_coords(m, 3);
        }
        // spike, superspike, explosion, tarexplosion, wizspike, knightspike,
        // lavasplash, teleport, lightningblood: position only
        0 | 1 | 3 | 4 | 7 | 8 | 10 | 11 | 13 => ctx.skip_coords(m, 3),
        _ => return Err(format!("unknown temp entity {te}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mvd_frame(msec: u8, cmd: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![msec, cmd];
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn serverdata(float_coords: bool) -> Vec<u8> {
        let mut p = vec![SVC_SERVERDATA];
        if float_coords {
            p.extend_from_slice(&PROTOCOL_VERSION_FTE.to_le_bytes());
            p.extend_from_slice(&FTE_PEXT_FLOATCOORDS.to_le_bytes());
        }
        p.extend_from_slice(&28i32.to_le_bytes());
        p.extend_from_slice(&1i32.to_le_bytes()); // server count
        p.extend_from_slice(b"qw\0");
        p.extend_from_slice(&0f32.to_le_bytes()); // demo time
        p.extend_from_slice(b"Test Level\0");
        p.extend_from_slice(&[0u8; 40]); // movevars
        p
    }

    #[test]
    fn decodes_a_minimal_mvd() {
        let mut demo = mvd_frame(0, DEM_ALL, &serverdata(true));

        let mut info = vec![SVC_UPDATEUSERINFO, 0];
        info.extend_from_slice(&7i32.to_le_bytes());
        info.extend_from_slice(b"\\name\\Ranger\\team\\red\0");
        info.push(SVC_MODELLIST);
        info.push(0);
        info.extend_from_slice(b"maps/dm3.bsp\0progs/player.mdl\0\0");
        info.push(0);
        demo.extend(mvd_frame(0, DEM_ALL, &info));

        // Full origin + yaw, then a delta that only moves x.
        let mut p1 = vec![SVC_PLAYERINFO, 0];
        p1.extend_from_slice(
            &((DF_ORIGIN | DF_ORIGIN << 1 | DF_ORIGIN << 2 | DF_ANGLES << 1) as u16).to_le_bytes(),
        );
        p1.push(0); // frame
        for v in [100.0f32, 200.0, 24.0] {
            p1.extend_from_slice(&v.to_le_bytes());
        }
        p1.extend_from_slice(&16384u16.to_le_bytes()); // yaw 90
        demo.extend(mvd_frame(13, DEM_ALL, &p1));

        let mut p2 = vec![SVC_PLAYERINFO, 0];
        p2.extend_from_slice(&(DF_ORIGIN as u16).to_le_bytes());
        p2.push(0);
        p2.extend_from_slice(&110.0f32.to_le_bytes());
        demo.extend(mvd_frame(13, DEM_ALL, &p2));

        // A hidden-message frame (dem_multiple to nobody) must be skipped.
        let mut hidden = vec![0, DEM_MULTIPLE];
        hidden.extend_from_slice(&0u32.to_le_bytes());
        hidden.extend_from_slice(&3u32.to_le_bytes());
        hidden.extend_from_slice(&[0xde, 0xad, 0xbe]);
        demo.extend(hidden);

        let (d, bad) = decode(&demo, "test.mvd").expect("parses");
        assert_eq!(bad, 0);
        assert_eq!(d.meta.map, "dm3");
        assert_eq!(d.meta.protocol, 28);
        let track = &d.mpd.tracks[&1];
        assert_eq!(
            track,
            &vec![(13, 100.0, 200.0, 24.0), (26, 110.0, 200.0, 24.0)]
        );
        assert_eq!(d.mpd.names[&1].name, "Ranger");
        assert!((d.mpd.yaws[&1][0].1 - 90.0).abs() < 0.01);
        // No recorder in an MVD: the first player's aim stands in as the camera stream.
        assert_eq!(d.mpd.view_angles, vec![(13, 0.0, 90.0)]);
    }

    #[test]
    fn short_coords_without_fte() {
        let mut demo = mvd_frame(0, DEM_ALL, &serverdata(false));
        let mut p = vec![SVC_PLAYERINFO, 2];
        p.extend_from_slice(&(DF_ORIGIN as u16).to_le_bytes());
        p.push(0);
        p.extend_from_slice(&(-80i16).to_le_bytes()); // -10.0 units
        demo.extend(mvd_frame(5, DEM_ALL, &p));
        let (d, bad) = decode(&demo, "x.mvd").expect("parses");
        assert_eq!(bad, 0);
        assert_eq!(d.mpd.tracks[&3][0], (5, -10.0, 0.0, 0.0));
    }

    fn qwd_frame(time: f32, cmd: u8, body: &[u8]) -> Vec<u8> {
        let mut v = time.to_le_bytes().to_vec();
        v.push(cmd);
        v.extend_from_slice(body);
        v
    }

    /// dem_read body: u32 length + netchan packet (8-byte sequence header + messages).
    fn qwd_read(time: f32, messages: &[u8]) -> Vec<u8> {
        let mut packet = vec![0u8; 8];
        packet.extend_from_slice(messages);
        let mut body = (packet.len() as u32).to_le_bytes().to_vec();
        body.extend_from_slice(&packet);
        qwd_frame(time, DEM_READ, &body)
    }

    fn qwd_serverdata(protover: i32, playernum: u8) -> Vec<u8> {
        let mut p = vec![SVC_SERVERDATA];
        p.extend_from_slice(&protover.to_le_bytes());
        p.extend_from_slice(&1i32.to_le_bytes()); // server count
        p.extend_from_slice(b"qw\0");
        p.push(playernum);
        p.extend_from_slice(b"The Slipgate Complex\0");
        p.extend_from_slice(&[0u8; 40]); // movevars
        p
    }

    fn coord(v: f32) -> [u8; 2] {
        ((v * 8.0) as i16).to_le_bytes()
    }

    #[test]
    fn decodes_a_minimal_qwd() {
        // dem_set, then a connectionless packet that must be ignored.
        let mut demo = qwd_frame(0.0, DEM_SET, &[0u8; 8]);
        let mut oob = 8u32.to_le_bytes().to_vec();
        oob.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, b'j', 0, 0, 0]);
        demo.extend(qwd_frame(0.0, DEM_READ, &oob));

        let mut setup = qwd_serverdata(28, 3);
        setup.push(SVC_UPDATEUSERINFO);
        setup.push(3);
        setup.extend_from_slice(&42i32.to_le_bytes());
        setup.extend_from_slice(b"\\name\\Recorder\0");
        setup.push(SVC_MODELLIST);
        setup.push(0);
        setup.extend_from_slice(b"maps/e1m1.bsp\0\0");
        setup.push(0);
        demo.extend(qwd_read(0.0, &setup));

        // Recorder usercmd: 24-byte usercmd_t, then pitch / yaw / roll floats.
        let mut cmd = vec![0u8; 24];
        for v in [10.0f32, 45.0, 0.0] {
            cmd.extend_from_slice(&v.to_le_bytes());
        }
        demo.extend(qwd_frame(0.5, DEM_CMD, &cmd));

        // Playerinfo with msec, a delta usercmd and one velocity component.
        let mut p1 = vec![SVC_PLAYERINFO, 3];
        p1.extend_from_slice(&((PF_MSEC | PF_COMMAND | PF_VELOCITY1) as u16).to_le_bytes());
        for v in [64.0, -32.5, 24.0] {
            p1.extend_from_slice(&coord(v));
        }
        p1.push(0); // frame
        p1.push(13); // msec
        p1.push(CM_ANGLE1 | CM_ANGLE2 | CM_FORWARD | CM_BUTTONS);
        p1.extend_from_slice(&0u16.to_le_bytes()); // pitch 0
        p1.extend_from_slice(&32768u16.to_le_bytes()); // yaw 180
        p1.extend_from_slice(&320i16.to_le_bytes()); // forwardmove
        p1.push(1); // buttons
        p1.push(13); // usercmd msec
        p1.extend_from_slice(&100i16.to_le_bytes()); // velocity x
        // Trailing message in the same packet must still be read.
        p1.extend_from_slice(&[SVC_UPDATEFRAGS, 3, 5, 0]);
        demo.extend(qwd_read(0.5, &p1));

        // A second update: player is dead, no usercmd this time.
        let mut p2 = vec![SVC_PLAYERINFO, 3];
        p2.extend_from_slice(&(PF_DEAD as u16).to_le_bytes());
        for v in [80.0, -32.5, 24.0] {
            p2.extend_from_slice(&coord(v));
        }
        p2.push(0);
        p2.push(SVC_DISCONNECT);
        demo.extend(qwd_read(1.25, &p2));

        // Anything after the disconnect is ignored.
        demo.extend(qwd_read(2.0, &[SVC_BAD]));

        let (d, bad) = decode(&demo, "test.qwd").expect("parses");
        assert_eq!(bad, 0);
        assert_eq!(d.meta.map, "e1m1");
        assert_eq!(d.meta.protocol, 28);
        assert_eq!(d.meta.client, "Recorder");
        assert_eq!(d.meta.server, "The Slipgate Complex"); // level name, no hostname set
        assert_eq!(d.mpd.primary_entity, Some(4));
        assert_eq!(
            d.mpd.tracks[&4],
            vec![(500, 64.0, -32.5, 24.0), (1250, 80.0, -32.5, 24.0)]
        );
        assert!((d.mpd.yaws[&4][0].1 - 180.0).abs() < 0.01);
        assert_eq!(d.mpd.view_angles, vec![(500, 10.0, 45.0)]);
        assert_eq!(d.mpd.life_states[&4], vec![(500, 0), (1250, 1)]);
        assert_eq!(d.mpd.duration, 1.25);
    }

    #[test]
    fn qwd_protocol_26_usercmd_layout() {
        // Before protocol 27: yaw is always sent, moves are bytes, msec is optional.
        let mut demo = qwd_read(0.0, &qwd_serverdata(26, 0));
        let mut p = vec![SVC_PLAYERINFO, 0];
        p.extend_from_slice(&(PF_COMMAND as u16).to_le_bytes());
        for v in [1.0, 2.0, 3.0] {
            p.extend_from_slice(&coord(v));
        }
        p.push(0); // frame
        p.push(CM_FORWARD | CM_SIDE); // no CM_ANGLE2 (=msec bit) here
        p.extend_from_slice(&16384u16.to_le_bytes()); // yaw 90, always present
        p.push(40); // forward (byte)
        p.push(0xd8); // side (byte)
        p.extend_from_slice(&[SVC_UPDATEPL, 0, 0]);
        demo.extend(qwd_read(0.1, &p));

        // Same layout, but with the optional msec byte flagged by bit 7.
        let mut p = vec![SVC_PLAYERINFO, 0];
        p.extend_from_slice(&(PF_COMMAND as u16).to_le_bytes());
        for v in [5.0, 2.0, 3.0] {
            p.extend_from_slice(&coord(v));
        }
        p.push(0); // frame
        p.push(CM_ANGLE2); // here: msec present
        p.extend_from_slice(&0u16.to_le_bytes()); // yaw 0
        p.push(13); // msec
        p.extend_from_slice(&[SVC_UPDATEPL, 0, 0]);
        demo.extend(qwd_read(0.2, &p));

        let (d, bad) = decode(&demo, "old.qwd").expect("parses");
        assert_eq!(bad, 0);
        assert_eq!(
            d.mpd.tracks[&1],
            vec![(100, 1.0, 2.0, 3.0), (200, 5.0, 2.0, 3.0)]
        );
        assert!((d.mpd.yaws[&1][0].1 - 90.0).abs() < 0.01);
    }

    #[test]
    fn rejects_files_without_serverdata() {
        assert!(parse(&[0u8; 16], "junk.mvd").is_err());
    }
}
