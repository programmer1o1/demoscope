// High-level extractor: walks the demo, finds the DEM_DATATABLES packet,
// processes svc_PacketEntities messages inside game packets, and produces
// per-entity position + life-state tracks plus userinfo metadata.

use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::bitreader::BitReader;
use super::datatable::{self, DataTables};
use super::packetentities::{parse_entity_updates, EntityWorld};
use super::stringtable::{parse_userinfo, PlayerInfo};


const HEADER_SIZE: usize = 1072;
const DEMOCMDINFO_SIZE: usize = 76;

// Demo packet command IDs (HL2DEMO format)
const DEM_SIGNON: u8 = 1;
const DEM_PACKET: u8 = 2;
const DEM_SYNCTICK: u8 = 3;
const DEM_CONSOLECMD: u8 = 4;
const DEM_USERCMD: u8 = 5;
const DEM_DATATABLES: u8 = 6;
const DEM_STOP: u8 = 7;
const DEM_STRINGTABLES: u8 = 8;

/// Per-entity extracted tracks.
#[derive(Default)]
pub struct PlayerTrackData {
    pub map: String,
    pub server: String,
    pub duration: f32,
    pub ticks: i32,
    pub demo_protocol: i32,
    pub net_protocol: i32,
    pub client_name: String,
    pub game_dir: String,
    pub tracks: HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    pub life_states: HashMap<u32, Vec<(i32, u8)>>,
    /// Observer-mode stream per entity (tick, mode). mode 0 = playing, anything
    /// else = spectating; used to break the path line + hide the avatar while a
    /// player's m_vecOrigin is tracking whoever they're watching.
    pub observer_modes: HashMap<u32, Vec<(i32, u8)>>,
    /// Eye-angle yaw per entity per tick (degrees, Source convention). Drives
    /// the local-frame WSAD reconstruction for non-recorder primaries.
    pub yaws: HashMap<u32, Vec<(i32, f32, f32)>>,
    /// Active-weapon entity id stream per player entity. Combined with the
    /// `m_iClassname` lookup, this resolves to a weapon name (rocketlauncher,
    /// scattergun, etc.) at any tick - enables per-shot weapon labels in the
    /// fire markers + much richer kill-feed correlation.
    pub weapons: HashMap<u32, Vec<(i32, i32)>>,
    /// Weapon entity id → class name (e.g. 47 → "CTFRocketLauncher"). Lets
    /// the HTML resolve `weapons[eid][i] = wep_eid` to a human-readable name.
    pub weapon_classes: HashMap<i32, String>,
    pub names: HashMap<u32, PlayerInfo>,
}

fn le_i32(d: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(d[off..off + 4].try_into().unwrap())
}
fn le_f32(d: &[u8], off: usize) -> f32 {
    f32::from_le_bytes(d[off..off + 4].try_into().unwrap())
}
fn read_cstring(data: &[u8], off: usize, max: usize) -> String {
    let end = data[off..off + max].iter().position(|&b| b == 0).unwrap_or(max);
    String::from_utf8_lossy(&data[off..off + end]).into_owned()
}

pub fn extract(dem_path: &Path) -> Result<PlayerTrackData, Box<dyn Error>> {
    let mut f = File::open(dem_path)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    extract_from_bytes(&bytes)
}

/// Byte-slice variant of `extract`. The CLI's `extract` is now a thin wrapper
/// around this; WASM calls it directly with a buffer from `FileReader`.
pub fn extract_from_bytes(data: &[u8]) -> Result<PlayerTrackData, Box<dyn Error>> {
    if data.len() < HEADER_SIZE || &data[0..8] != b"HL2DEMO\0" {
        return Err("not a valid HL2DEMO file".into());
    }
    let mut out = PlayerTrackData::default();
    out.demo_protocol = le_i32(&data, 8);
    out.net_protocol  = le_i32(&data, 12);
    out.server        = read_cstring(&data, 16, 260);
    out.client_name   = read_cstring(&data, 276, 260);
    out.map           = read_cstring(&data, 536, 260);
    out.game_dir      = read_cstring(&data, 796, 260);
    out.duration      = le_f32(&data, 1056);
    out.ticks         = le_i32(&data, 1060);
    // Header carries sign_on_length at offset 1068 - currently unused since
    // we walk the signon section packet-by-packet (with splitscreen-aware
    // preamble for proto-4) rather than fast-forwarding past it.
    let _sign_on_length = le_i32(&data, 1068) as i64;

    let extra: usize = if out.demo_protocol > 3 { 1 } else { 0 };
    let pkt_hdr = 5 + extra;

    let mut data_tables: Option<DataTables> = None;
    let mut world: Option<EntityWorld> = None;
    let mut last_pos: HashMap<u32, (f32, f32, f32)> = HashMap::new();
    let mut origin_state: HashMap<u32, OriginTracker> = HashMap::new();
    let mut last_life: HashMap<u32, u8> = HashMap::new();
    let mut last_obs: HashMap<u32, u8> = HashMap::new();
    let mut last_yaw: HashMap<u32, (f32, f32)> = HashMap::new();
    let mut last_weapon: HashMap<u32, i32> = HashMap::new();
    let mut userinfo_table_id: Option<usize> = None;

    let mut offset = HEADER_SIZE;
    // Proto-4 (L4D / L4D2 / Portal 2 / Stanley Parable / old CS:GO) replaced
    // the single 76-byte `democmdinfo_t` in each SIGNON/PACKET preamble with
    // an array of `Split_t[MAX_SPLITSCREEN_CLIENTS]`. The constant varies by
    // game - Portal 2 / Stanley / CS:GO ship with 2 splitscreen slots; L4D1
    // / L4D2 ship with 4. There's no field in the demo header that tells us
    // which, so we detect it from the first packet by trying N = 1, 2, 4 and
    // picking the one whose length-field reads as a plausible payload size.
    // Confirmed against the Alien Swarm SDK 2009 `demoformat.h` reference.
    // Portal 2-engine games (Portal 2, Aperture Tag, Stanley, etc.) always ship
    // MAX_SPLITSCREEN_CLIENTS = 2; only L4D1/L4D2 use 4. Knowing the game pins
    // the count exactly, which is far more reliable than the length-probe below
    // - that heuristic can false-positive (a puzzlemaker-export demo probed as
    // N=4 and desynced on the first packet). Fall back to probing only for
    // proto-4 games we can't identify.
    let portal2_engine = datatable::DataTableQuirks::for_game(&out.game_dir).portal2_extra_bits;
    let mut splitscreen_count: usize = 1;
    if out.demo_protocol > 3 {
        if portal2_engine {
            splitscreen_count = 2;
        } else if data.len() > HEADER_SIZE + 5 {
            let pkt_start = HEADER_SIZE + pkt_hdr;
            for n in [4, 2, 1] {
                let len_off = pkt_start + 76 * n + 8;
                if len_off + 4 > data.len() { continue; }
                let length = le_i32(&data, len_off);
                let payload_end = len_off + 4 + length as usize;
                if length > 0 && (length as usize) < (data.len() - pkt_start) && payload_end < data.len() {
                    splitscreen_count = n;
                    break;
                }
            }
        }
    }
    let democmdinfo_bytes = 76 * splitscreen_count;
    // Proto-4 (L4D / Portal 2 / Stanley Parable, etc.) shifted the DEM_*
    // command IDs upward by one starting at value 8: cmd=8 became a new
    // DEM_CUSTOMDATA, and the slot that proto-3 used for DEM_STRINGTABLES is
    // now at cmd=9. Map back to the proto-3 semantic codes here so the rest
    // of the match arms don't have to care which protocol they're seeing.
    let p4 = out.demo_protocol > 3;
    // Portal 2 engine renumbers the net-message IDs (NetSplitScreenUser at 3,
    // SvcSplitScreen at 22, SvcPrint moved 7→16, NetTick/StringCmd/SetConVar/
    // SignonState each shift down one). scan_game_payload needs this flag to
    // dispatch correctly. (`portal2_engine` computed above for splitscreen.)
    //
    // Debug aids for porting new proto-4 games (Stanley Parable, L4D2 …) - set
    // the env var to trace where the demo-command walk desyncs. See README
    // "Investigating Stanley Parable & L4D2". DUMP_SCAN=1 prints the per-packet
    // walk + whether DEM_DATATABLES is reached and parses.
    let dbg_scan = std::env::var("DUMP_SCAN").is_ok();
    if dbg_scan {
        eprintln!("[SCAN] proto={} net={} game={} portal2_engine={} splitscreen={}",
            out.demo_protocol, out.net_protocol, out.game_dir, portal2_engine, splitscreen_count);
    }
    let mut dbg_pkts = 0u32;
    while offset < data.len() {
        if offset + 5 > data.len() { break; }
        let raw_cmd = data[offset];
        let tick = le_i32(&data, offset + 1);
        offset += pkt_hdr;
        let cmd = if p4 {
            match raw_cmd {
                9 => DEM_STRINGTABLES, // shifted from 8
                8 => 99,               // DEM_CUSTOMDATA - we just want to skip it
                v => v,
            }
        } else { raw_cmd };

        match cmd {
            DEM_STOP => break,
            DEM_SYNCTICK => { /* no payload */ }
            DEM_SIGNON | DEM_PACKET => {
                if offset + democmdinfo_bytes + 12 > data.len() { break; }
                let length = le_i32(&data, offset + democmdinfo_bytes + 8) as usize;
                let payload_start = offset + democmdinfo_bytes + 12;
                let payload_end = payload_start + length;
                if payload_end > data.len() { break; }
                dbg_pkts += 1;
                scan_game_payload(
                    &data[payload_start..payload_end],
                    tick,
                    out.demo_protocol,
                    portal2_engine,
                    data_tables.as_ref(),
                    world.as_mut(),
                    &mut last_pos, &mut origin_state, &mut last_life, &mut last_obs, &mut last_yaw, &mut last_weapon,
                    &mut out.tracks, &mut out.life_states, &mut out.observer_modes, &mut out.yaws, &mut out.weapons,
                    &mut out.weapon_classes,
                    userinfo_table_id,
                    &mut out.names,
                );
                offset = payload_end;
            }
            DEM_CONSOLECMD => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(&data, offset) as usize;
                offset += 4 + length;
            }
            DEM_USERCMD => {
                if offset + 8 > data.len() { break; }
                let length = le_i32(&data, offset + 4) as usize;
                offset += 8 + length;
            }
            DEM_DATATABLES => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(&data, offset) as usize;
                let payload_start = offset + 4;
                let payload_end = (payload_start + length).min(data.len());
                let quirks = datatable::DataTableQuirks::for_game(&out.game_dir);
                if let Some(dt) = datatable::parse(&data[payload_start..payload_end], out.demo_protocol, quirks) {
                    // DUMP_FLAT=<class_id> dumps that server class's flattened
                    // prop list (name / type / bits / priority / changes-often)
                    // for diffing against an engine bit-trace. DUMP_FLAT=0 just
                    // lists the server classes so you can find the player id.
                    if let Ok(want) = std::env::var("DUMP_FLAT") {
                        for c in &dt.server_classes {
                            eprintln!("[CLASS] id={} name={} dt={}", c.id, c.name, c.data_table);
                        }
                        if let Ok(id) = want.parse::<u16>() {
                            if let Some(flat) = dt.flat_props.get(&id) {
                                eprintln!("[FLAT] class {} has {} props", id, flat.len());
                                for (i, p) in flat.iter().enumerate() {
                                    let co = if p.flags & super::sendprop::SPROP_CHANGES_OFTEN != 0 { "CO" } else { "" };
                                    eprintln!("[FLAT] {:>3} {:?} nbits={} prio={} {} {}",
                                        i, p.prop_type, p.bit_count, p.priority, co, p.name);
                                }
                            }
                        }
                    }
                    if dbg_scan {
                        eprintln!("[SCAN] DATATABLES parsed at pkt {}: {} classes, {} flat arrays",
                            dbg_pkts, dt.server_classes.len(), dt.flat_props.len());
                    }
                    world = Some(EntityWorld::new(&dt));
                    data_tables = Some(dt);
                } else if dbg_scan {
                    eprintln!("[SCAN] DATATABLES parse FAILED (returned None) at offset {:#x}", offset);
                }
                offset = payload_end;
            }
            99 => {
                // DEM_CUSTOMDATA (proto-4 only) - length-prefixed payload we
                // don't need to interpret. Skip past it cleanly so the rest
                // of the packet stream remains aligned.
                if offset + 8 > data.len() { break; }
                let _id = le_i32(&data, offset);
                let length = le_i32(&data, offset + 4) as usize;
                offset = (offset + 8 + length).min(data.len());
            }
            DEM_STRINGTABLES => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(&data, offset) as usize;
                let payload_start = offset + 4;
                let payload_end = (payload_start + length).min(data.len());
                if let Some(parsed) = parse_userinfo(&data[payload_start..payload_end]) {
                    out.names.extend(parsed.players);
                    userinfo_table_id = parsed.table_names.iter()
                        .position(|n| n == "userinfo");
                }
                offset = payload_end;
            }
            _ => {
                if dbg_scan {
                    eprintln!("[SCAN] abort at offset {:#x}: unknown cmd raw={} mapped={} after {} game pkts (data_tables={})",
                        offset, raw_cmd, cmd, dbg_pkts, data_tables.is_some());
                }
                break;
            }
        }
    }
    if dbg_scan {
        eprintln!("[SCAN] done: {} game packets, data_tables={}, entities={}",
            dbg_pkts, data_tables.is_some(),
            world.as_ref().map(|w| w.entities.len()).unwrap_or(0));
    }

    Ok(out)
}

// Scan the game-packet payload for svc_PacketEntities (type 26). Every other
// known message type is skipped using its length field; unknown types abort
// the scan since we can't advance the bit cursor past them.
fn scan_game_payload(
    payload: &[u8],
    tick: i32,
    demo_protocol: i32,
    portal2: bool,
    data: Option<&DataTables>,
    mut world: Option<&mut EntityWorld>,
    last_pos: &mut HashMap<u32, (f32, f32, f32)>,
    origin_state: &mut HashMap<u32, OriginTracker>,
    last_life: &mut HashMap<u32, u8>,
    last_obs: &mut HashMap<u32, u8>,
    last_yaw: &mut HashMap<u32, (f32, f32)>,
    last_weapon: &mut HashMap<u32, i32>,
    tracks: &mut HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    life_states: &mut HashMap<u32, Vec<(i32, u8)>>,
    observer_modes: &mut HashMap<u32, Vec<(i32, u8)>>,
    yaws: &mut HashMap<u32, Vec<(i32, f32, f32)>>,
    weapons: &mut HashMap<u32, Vec<(i32, i32)>>,
    weapon_classes: &mut HashMap<i32, String>,
    userinfo_table_id: Option<usize>,
    names: &mut HashMap<u32, PlayerInfo>,
) {
    let mut br = BitReader::new(payload);
    let total_bits = payload.len() * 8;

    macro_rules! tryread { ($e:expr) => { match $e { Some(v) => v, None => return } } }

    while br.bit_pos() + 6 <= total_bits {
        let msg_type_raw = tryread!(br.read_bits(6));
        // Portal 2 engine renumbers the message IDs. Handle its two new
        // message types inline, then remap the shifted ones back to our
        // canonical (HL2 / Source 2007) IDs so the match below is shared.
        // Reference: NeKzor/sdp NetMessages.ts Portal2Engine table.
        if portal2 {
            match msg_type_raw {
                3 => { // NetSplitScreenUser: 1 bit
                    if !br.skip(1) { return; }
                    continue;
                }
                22 => { // SvcSplitScreen: 1 bit + 11-bit length + data
                    if !br.skip(1) { return; }
                    let len = tryread!(br.read_bits(11));
                    if !br.skip(len) { return; }
                    continue;
                }
                _ => {}
            }
        }
        let msg_type = if portal2 {
            match msg_type_raw {
                4 => 3,   // NetTick
                5 => 4,   // NetStringCmd
                6 => 5,   // NetSetConVar
                7 => 6,   // NetSignonState
                16 => 7,  // SvcPrint
                v => v,
            }
        } else { msg_type_raw };
        match msg_type {
            0  => { /* net_NOP */ }
            3  => { if !br.skip(64) { return; } } // net_Tick
            4  => { if br.read_cstring(512).is_none() { return; } } // net_StringCmd
            5  => {
                let count = tryread!(br.read_bits(8)) as usize;
                for _ in 0..count {
                    if br.read_cstring(256).is_none() || br.read_cstring(256).is_none() { return; }
                }
            }
            6  => { if !br.skip(40) { return; } } // net_SignonState
            7  => { /* svc_Print (string) */ if br.read_cstring(2048).is_none() { return; } }
            8  => { /* svc_ServerInfo - layout varies by demo_protocol.
                       Proto-3 (TF2 / CS:S net=24):
                         version(16) + server_count(32) + stv(1) + dedicated(1)
                         + max_crc(32) + max_classes(16)             = 98 bits
                         + map_hash(128) + player_slot(8) + max_players(8)
                         + interval_per_tick(32) + platform(8)       = 184 bits
                       Proto-4 (Portal 2 / L4D / Stanley, isNewEngine):
                         version(16) + server_count(32) + stv(1) + dedicated(1)
                         + max_crc(32) + max_classes(16) + map_crc(32)
                         + player_slot(8) + max_players(8) + unk(32)
                         + interval_per_tick(32) + platform(8)       = 218 bits
                       (Reference: NeKzor/sdp NetMessages.ts SvcServerInfo.) */
                let fixed = if demo_protocol >= 4 { 218 } else { 282 };
                if !br.skip(fixed) { return; }
                if br.read_cstring(260).is_none() { return; } // game
                if br.read_cstring(260).is_none() { return; } // map
                if br.read_cstring(260).is_none() { return; } // skybox
                if br.read_cstring(260).is_none() { return; } // server_name
                if !br.skip(1) { return; } // replay
            }
            9  => { /* svc_SendTable - shouldn't appear mid-game */ return; }
            10 => { /* svc_ClassInfo */
                let n = tryread!(br.read_bits(16));
                let create = tryread!(br.read_bool());
                if !create {
                    let bits = bits_for(data.map(|d| d.server_classes.len() as u32).unwrap_or(1));
                    for _ in 0..n {
                        if !br.skip(bits) { return; }
                        if br.read_cstring(256).is_none() { return; }
                        if br.read_cstring(256).is_none() { return; }
                    }
                }
            }
            11 => { if !br.skip(1) { return; } } // svc_SetPause
            12 => { /* svc_CreateStringTable - bit format is fiddly; we don't
                       rely on it for userinfo (DEM_STRINGTABLES gives us the
                       table_id by position). Just consume bits via length. */
                if br.read_cstring(256).is_none() { return; }
                if !br.skip(16) { return; }
                let _ = tryread!(br.read_var_u32());
                let length = tryread!(br.read_bits(20)) as usize;
                if !br.skip(1) { return; }
                if !br.skip(length as u32) { return; }
            }
            13 => { /* svc_UpdateStringTable: table_id(5) + has_changed(1)
                       + [num_changed(16) if has_changed] + length(20) + data */
                let table_id = tryread!(br.read_bits(5)) as usize;
                let has_changed = tryread!(br.read_bool());
                let num_changed = if has_changed { tryread!(br.read_bits(16)) } else { 1 };
                let length = tryread!(br.read_bits(20)) as usize;
                let data_start = br.bit_pos();
                // The userinfo table_id is known from DEM_STRINGTABLES. Only
                // userinfo carries rename-worthy data; ignore everything else.
                if Some(table_id) == userinfo_table_id {
                    apply_userinfo_update(payload, data_start, length, num_changed, names);
                }
                if !br.skip(length as u32) { return; }
            }
            14 => { /* svc_VoiceInit */
                if br.read_cstring(256).is_none() { return; }
                if !br.skip(8) { return; }
            }
            15 => {
                if !br.skip(16) { return; }
                let length = tryread!(br.read_bits(16)) as usize;
                if !br.skip(length as u32) { return; }
            }
            16 => { /* svc_HLTV control */ return; }
            17 => { /* svc_Sounds:
                       reliable(1) + (if reliable: length(8); else num(8) + length(16)) + data[length bits] */
                let reliable = tryread!(br.read_bool());
                let length = if reliable {
                    tryread!(br.read_bits(8)) as usize
                } else {
                    if !br.skip(8) { return; }
                    tryread!(br.read_bits(16)) as usize
                };
                if !br.skip(length as u32) { return; }
            }
            18 => { if !br.skip(11) { return; } }
            19 => { if !br.skip(49) { return; } }
            20 => { if !br.skip(48) { return; } }
            21 => { return; } // svc_BSPDecal
            22 => {
                if !br.skip(1) { return; }
                let length = tryread!(br.read_bits(11)) as usize;
                if !br.skip(length as u32) { return; }
            }
            23 => {
                let _ = tryread!(br.read_bits(8));
                let length = tryread!(br.read_bits(11)) as usize;
                if !br.skip(length as u32) { return; }
            }
            24 => {
                if !br.skip(20) { return; }
                let length = tryread!(br.read_bits(11)) as usize;
                if !br.skip(length as u32) { return; }
            }
            25 => {
                let length = tryread!(br.read_bits(11)) as usize;
                if !br.skip(length as u32) { return; }
            }
            26 => {
                let _max_entries = tryread!(br.read_bits(11));
                let has_delta = tryread!(br.read_bool());
                if has_delta { if !br.skip(32) { return; } }
                if !br.skip(1) { return; } // base_line
                let num_changed = tryread!(br.read_bits(11));
                let length_bits = tryread!(br.read_bits(20)) as usize;
                if !br.skip(1) { return; } // updated_base_line
                let payload_start_bit = br.bit_pos();
                if let (Some(dt), Some(w)) = (data, world.as_deref_mut()) {
                    let r = parse_entity_updates(
                        payload, payload_start_bit, length_bits,
                        num_changed, has_delta, w, dt,
                        demo_protocol >= 4,
                    );
                    if std::env::var("DUMP_ENT").is_ok() {
                        use std::collections::BTreeMap;
                        let mut by_class: BTreeMap<u16, usize> = BTreeMap::new();
                        for s in w.entities.values() { *by_class.entry(s.class_id).or_default() += 1; }
                        let names: Vec<String> = by_class.iter().take(8).map(|(cid, n)| {
                            let nm = dt.server_classes.iter().find(|c| c.id == *cid).map(|c| c.name.as_str()).unwrap_or("?");
                            format!("{}×{}:{}", n, cid, nm)
                        }).collect();
                        eprintln!("[ENT] t={} delta={} maxE={} updates={} lenbits={} decode={} world={} eids[{}..] classes: {}",
                            tick, has_delta, _max_entries, num_changed, length_bits, if r.is_some() {"ok"} else {"NONE"},
                            w.entities.len(),
                            w.entities.keys().min().copied().unwrap_or(0),
                            names.join(" "));
                    }
                    if !br.skip(length_bits as u32) { return; }
                    scrape_player_state(tick, w, dt, last_pos, origin_state, last_life, last_obs, last_yaw, last_weapon,
                        tracks, life_states, observer_modes, yaws, weapons, weapon_classes);
                } else {
                    if !br.skip(length_bits as u32) { return; }
                }
            }
            27 => {
                if !br.skip(9) { return; }
                let length = tryread!(br.read_bits(17)) as usize;
                if !br.skip(length as u32) { return; }
            }
            28 => { if !br.skip(14) { return; } }
            29 => {
                if !br.skip(16) { return; }
                let length = tryread!(br.read_bits(16)) as usize;
                if !br.skip(length as u32 * 8) { return; }
            }
            30 => {
                let _ = tryread!(br.read_bits(9));
                let total_length = tryread!(br.read_bits(20)) as usize;
                if !br.skip(total_length as u32) { return; }
            }
            31 => {
                if !br.skip(32) { return; }
                if br.read_cstring(256).is_none() { return; }
            }
            32 => {
                // svc_CmdKeyValues: length(32) + data[length bytes]
                let length = tryread!(br.read_bits(32)) as u32;
                if !br.skip(length.wrapping_mul(8)) { return; }
            }
            33 => {
                // svc_PaintMapData (Portal 2 family) - int32 byte length then
                // that many bits of paint data. Skip entirely.
                let length = tryread!(br.read_bits(32)) as u32;
                if !br.skip(length) { return; }
            }
            _ => return, // unknown - bail
        }
    }
}

fn bits_for(n: u32) -> u32 {
    let mut bits = 0;
    while (1u32 << bits) < n { bits += 1; }
    bits.max(1)
}

/// floor(log2(n)) for n >= 1. Used by Source for string-table entry index widths.
fn log2(n: u32) -> u32 {
    31 - n.leading_zeros()
}

/// Decode a svc_UpdateStringTable diff against the userinfo table and merge
/// any changed entries into `names`. The userinfo table in Source 1 always
/// has max_entries = 256 (= MAX_PLAYERS), so entry_bits = 8. Userdata is
/// variable-size (player_info_t).
///
/// Wire format per changed entry:
///   next_entry (1 bit) - if 1, entry = last+1; else read entry_bits absolute
///   has_string (1 bit) - if 1: substring_flag(1) + [if substring: idx(5)+nchars(5)] + cstring suffix
///   has_userdata (1 bit) - if 1: 14-bit byte count + bytes (var size)
fn apply_userinfo_update(
    payload: &[u8],
    data_start_bit: usize,
    length_bits: usize,
    num_changed: u32,
    names: &mut HashMap<u32, PlayerInfo>,
) {
    const ENTRY_BITS: u32 = 8;
    let mut br = BitReader::new(payload);
    if !br.skip(data_start_bit as u32) { return; }
    let max_pos = data_start_bit + length_bits;
    let mut last_entry: i32 = -1;
    for _ in 0..num_changed {
        if br.bit_pos() >= max_pos { return; }
        let next = match br.read_bool() { Some(b) => b, None => return };
        let entry: i32 = if next {
            last_entry + 1
        } else {
            match br.read_bits(ENTRY_BITS) { Some(v) => v as i32, None => return }
        };
        last_entry = entry;

        let has_string = match br.read_bool() { Some(b) => b, None => return };
        if has_string {
            let is_substring = match br.read_bool() { Some(b) => b, None => return };
            if is_substring {
                if !br.skip(5 + 5) { return; }
            }
            if br.read_cstring(1024).is_none() { return; }
        }

        let has_userdata = match br.read_bool() { Some(b) => b, None => return };
        if has_userdata {
            let nbytes = match br.read_bits(14) { Some(v) => v as usize, None => return };
            let mut bytes = Vec::with_capacity(nbytes);
            for _ in 0..nbytes {
                let b = match br.read_bits(8) { Some(v) => v as u8, None => return };
                bytes.push(b);
            }
            if let Some(mut pi) = super::stringtable::parse_player_info_blob(&bytes) {
                let entity_id = (entry as u32) + 1;
                // Preserve every prior alias for this slot, then add the new one.
                if let Some(prev) = names.get(&entity_id) {
                    pi.aliases = prev.aliases.clone();
                }
                if !pi.aliases.iter().any(|a| a == &pi.name) {
                    pi.aliases.push(pi.name.clone());
                }
                names.insert(entity_id, pi);
            }
        }
    }
}

/// Per-entity bookkeeping for picking the live m_vecOrigin source. `changes[s]`
/// counts how many times candidate slot `s` (local-exclusive vs non-local copy)
/// has actually moved; `last[s]` is its previous value for change detection.
#[derive(Default, Clone)]
pub struct OriginTracker {
    last: Vec<Option<(f32, f32, f32)>>,
    changes: Vec<u32>,
}

fn scrape_player_state(
    tick: i32,
    world: &EntityWorld,
    data: &DataTables,
    last_pos: &mut HashMap<u32, (f32, f32, f32)>,
    origin_state: &mut HashMap<u32, OriginTracker>,
    last_life: &mut HashMap<u32, u8>,
    last_obs: &mut HashMap<u32, u8>,
    last_yaw: &mut HashMap<u32, (f32, f32)>,
    last_weapon: &mut HashMap<u32, i32>,
    tracks: &mut HashMap<u32, Vec<(i32, f32, f32, f32)>>,
    life_states: &mut HashMap<u32, Vec<(i32, u8)>>,
    observer_modes: &mut HashMap<u32, Vec<(i32, u8)>>,
    yaws: &mut HashMap<u32, Vec<(i32, f32, f32)>>,
    weapons: &mut HashMap<u32, Vec<(i32, i32)>>,
    weapon_classes: &mut HashMap<i32, String>,
) {
    for (&eid, state) in &world.entities {
        if eid == 0 || eid > 64 { continue; }
        let class = match data.server_classes.iter().find(|c| c.id == state.class_id) {
            Some(c) => c,
            None => continue,
        };
        if !class.name.contains("Player") { continue; }
        let flat = match data.flat_props.get(&state.class_id) {
            Some(f) => f,
            None => continue,
        };

        // Find m_vecOrigin / m_vecOrigin[2] / m_lifeState / m_angEyeAngles[1]
        // / m_hActiveWeapon by name in the flat list. The eye-angles yaw is
        // what drives WASD input direction in Source. m_hActiveWeapon points
        // at the entity id of the wielded weapon - we resolve that to a class
        // name on the HTML side.
        let mut life = None;
        let mut yaw = None;
        let mut pitch = None;
        let mut wep_handle = None;
        // m_iObserverMode: 0 = not spectating; anything else = dead/spectating
        // (deathcam, chase, roaming, …). While observing, the engine streams
        // m_vecOrigin as the *spectated* target's position, so the value is
        // meaningless for this player - the HTML uses this stream to break the
        // path line and hide the avatar during those windows.
        let mut obs = None;
        // A player class carries more than one m_vecOrigin: the local-player-
        // exclusive copy (DT_LocalPlayerExclusive, earlier in the flat list)
        // and a non-local copy. The server only streams ONE of them per
        // entity - the local-exclusive one for the recorder, the non-local
        // one for everyone else - while the other stays frozen at its baseline
        // value. We can't tell which is live from a single tick (both are
        // present), so collect every origin candidate in flat order here and
        // let the per-entity tracker below pick whichever one is actually
        // moving. (Blindly taking the first froze all non-local players.)
        let mut origin_cands: Vec<(Option<f32>, Option<f32>, Option<f32>)> = Vec::new();
        for (i, p) in flat.iter().enumerate() {
            match p.name.as_str() {
                "m_vecOrigin" => {
                    let mut c = (None, None, None);
                    if let Some(v) = state.props.get(&i) {
                        if let Some((vx, vy)) = v.as_vector_xy() {
                            c.0 = Some(vx); c.1 = Some(vy);
                        } else if let Some((vx, vy, vz)) = v.as_vector() {
                            c = (Some(vx), Some(vy), Some(vz));
                        }
                    }
                    origin_cands.push(c);
                }
                "m_vecOrigin[2]" => {
                    // Pairs with the most recent m_vecOrigin (10↔11, 14↔15, …).
                    if let Some(v) = state.props.get(&i) {
                        if let Some(f) = v.as_f32() {
                            if let Some(last) = origin_cands.last_mut() { last.2 = Some(f); }
                        }
                    }
                }
                "m_lifeState" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(n) = v.as_i64() { life = Some(n as u8); }
                    }
                }
                "m_angEyeAngles[1]" | "m_angRotation[1]" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(f) = v.as_f32() { yaw = Some(f); }
                    }
                }
                // Pitch (look up/down). Needed to drive the first-person camera
                // on proto-4 demos, which have no usercmds - the playback
                // timeline is synthesized from this + yaw + the position track.
                "m_angEyeAngles[0]" | "m_angRotation[0]" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(f) = v.as_f32() { pitch = Some(f); }
                    }
                }
                "m_iObserverMode" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(n) = v.as_i64() { obs = Some(n as u8); }
                    }
                }
                "m_hActiveWeapon" => {
                    if let Some(v) = state.props.get(&i) {
                        if let Some(n) = v.as_i64() {
                            // EHANDLE: low 11 bits = entity index; serial in upper bits.
                            let ent_idx = (n as i32) & 0x7FF;
                            wep_handle = Some(ent_idx);
                        }
                    }
                }
                _ => {}
            }
        }

        let eid_u = eid as u32;

        // Pick the live origin source for this entity. We score each candidate
        // slot by how many times its value has changed so far; the server only
        // keeps one slot moving, so it pulls ahead within a few ticks and the
        // argmax locks onto it. (The first time a slot is seen doesn't count as
        // a change, so a baseline-only copy stays at zero and never wins.) On a
        // tie - including the very first tick, where both copies share the
        // baseline value - we prefer the earliest slot, matching the old
        // local-player behaviour.
        let tracker = origin_state.entry(eid_u).or_default();
        if tracker.last.len() < origin_cands.len() {
            tracker.last.resize(origin_cands.len(), None);
            tracker.changes.resize(origin_cands.len(), 0);
        }
        for (s, c) in origin_cands.iter().enumerate() {
            if let (Some(cx), Some(cy)) = (c.0, c.1) {
                let cz = c.2.unwrap_or(0.0);
                let moved = tracker.last[s].map_or(false, |(lx, ly, lz): (f32, f32, f32)| {
                    (lx - cx).abs() > 0.01 || (ly - cy).abs() > 0.01 || (lz - cz).abs() > 0.01
                });
                if moved { tracker.changes[s] += 1; }
                tracker.last[s] = Some((cx, cy, cz));
            }
        }
        let mut best = 0usize;
        for s in 1..tracker.changes.len() {
            if tracker.changes[s] > tracker.changes[best] { best = s; }
        }
        let (x, y, z) = match origin_cands.get(best) {
            Some(&(cx, cy, cz)) => (cx, cy, cz),
            None => (None, None, None),
        };

        let pos = last_pos.entry(eid_u).or_insert((0.0, 0.0, 0.0));
        let mut changed = false;
        if let Some(vx) = x { if pos.0 != vx { pos.0 = vx; changed = true; } }
        if let Some(vy) = y { if pos.1 != vy { pos.1 = vy; changed = true; } }
        if let Some(vz) = z { if pos.2 != vz { pos.2 = vz; changed = true; } }

        if changed {
            let mag2 = pos.0*pos.0 + pos.1*pos.1 + pos.2*pos.2;
            let bucket = tracks.entry(eid_u).or_default();
            let near_origin = mag2 < 16.0;
            let dedupe = bucket.last().map_or(false, |&(_, lx, ly, lz)| {
                let dx = pos.0 - lx; let dy = pos.1 - ly; let dz = pos.2 - lz;
                dx*dx + dy*dy + dz*dz < 1.0
            });
            if !(near_origin && bucket.is_empty()) && !dedupe {
                bucket.push((tick, pos.0, pos.1, pos.2));
            }
        }

        if let Some(ls) = life {
            if last_life.get(&eid_u).copied() != Some(ls) {
                last_life.insert(eid_u, ls);
                life_states.entry(eid_u).or_default().push((tick, ls));
            }
        }

        // Observer-mode transitions (analogous to life-state). Emitted only on
        // change so the stream stays tiny.
        if let Some(om) = obs {
            if last_obs.get(&eid_u).copied() != Some(om) {
                last_obs.insert(eid_u, om);
                observer_modes.entry(eid_u).or_default().push((tick, om));
            }
        }

        // Eye angles (yaw + pitch) - dedupe small changes (< 2° on either axis)
        // since the prop fires for every tiny mouse movement and would
        // otherwise be megabytes of noise. Emitted as (tick, yaw, pitch).
        if let Some(y_now) = yaw {
            let p_now = pitch.unwrap_or(0.0);
            let prev = last_yaw.get(&eid_u).copied();
            let should_emit = prev.map_or(true, |(py, pp)| (py - y_now).abs() >= 2.0 || (pp - p_now).abs() >= 2.0);
            if should_emit {
                last_yaw.insert(eid_u, (y_now, p_now));
                yaws.entry(eid_u).or_default().push((tick, y_now, p_now));
            }
        }

        // Active weapon entity id. Only emit on change to keep the stream
        // tiny - switching weapons is rare compared to ticks. Also record the
        // weapon entity's class name once seen, so the HTML side can resolve
        // ids → human-readable names like "CTFRocketLauncher".
        if let Some(w) = wep_handle {
            if last_weapon.get(&eid_u).copied() != Some(w) {
                last_weapon.insert(eid_u, w);
                weapons.entry(eid_u).or_default().push((tick, w));
                if w > 0 && w < u16::MAX as i32 && !weapon_classes.contains_key(&w) {
                    if let Some(wstate) = world.entities.get(&(w as u16)) {
                        if let Some(c) = data.server_classes.iter().find(|c| c.id == wstate.class_id) {
                            weapon_classes.insert(w, c.name.clone());
                        }
                    }
                }
            }
        }
    }
}
