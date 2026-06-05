// High-level extractor: walks the demo, finds the DEM_DATATABLES packet,
// processes svc_PacketEntities messages inside game packets, and produces
// per-entity position + life-state tracks plus userinfo metadata.

use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::csgo;
use super::datatable::{self, DataTables};
use super::packetentities::EntityWorld;
use super::stringtable::{parse_userinfo, PlayerInfo};

// Per-frame packet/svc scanning lives in the `scan` submodule; the entry points
// and the per-entity `OriginTracker` it fills are pulled back in here.
mod scan;
use scan::{scan_csgo_payload, scan_game_payload, OriginTracker};


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

/// Per-player economy + scoreboard snapshot (latest value wins). Mirrors the
/// Source 2 `PlayerEcon` so the CS:S / CS:GO (Source 1) path reaches parity with
/// CS2. `money` + `team` come off the player entity (`m_iAccount` /
/// `m_iTeamNum`); `kills` / `deaths` / `assists` / `score` / `mvps` come off the
/// `CCSPlayerResource` arrays (per-player-index `m_iScore[i]` etc.). CS:S has no
/// separate kills/assists arrays, so `kills` falls back to `score`.
#[derive(Default, Clone)]
pub struct PlayerEcon {
    pub money: i32,
    pub kills: i32,
    pub deaths: i32,
    pub assists: i32,
    pub score: i32,
    pub mvps: i32,
    pub team: i32,
}

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
    /// Per-player-entity economy + scoreboard (money / K / D / A / score / MVPs /
    /// team). Populated on the CS:S / CS:GO path; empty elsewhere.
    pub econ: HashMap<u32, PlayerEcon>,
    /// Recorder's per-frame camera angles (tick, pitch, yaw) straight from the
    /// democmdinfo viewAngles. Far denser + more accurate than the networked
    /// eye-angle SendProp - this is the engine's actual playback camera. Drives
    /// the FPS camera on demos without usercmds (Portal 2 etc.).
    pub view_angles: Vec<(i32, f32, f32)>,
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
    // Accept both the canonical HL2DEMO magic and Garry's Mod 13+'s renamed
    // GMODEMO (identical container - see is_source_demo_magic in main.rs).
    let magic_ok = data.len() >= 8 && (&data[0..8] == b"HL2DEMO\0" || &data[0..8] == b"GMODEMO\0");
    if data.len() < HEADER_SIZE || !magic_ok {
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
    // CS:GO protobuf string-table registry (persists across packets so
    // svc_UpdateStringTable diffs resolve against the right table).
    let mut csgo_string_tables = csgo::stringtables::StringTables::new();

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
    // Container quirks (message-ID remap + splitscreen=2) are Portal2-engine
    // only - NOT keyed off the SendProp flag format, which L4D also uses.
    let portal2_engine = datatable::is_portal2_engine(&out.game_dir);
    // L4D1/L4D2 use the SAME renumbered net-message map as the Portal 2 engine
    // (verified in L4D1 engine.dll: NET_Tick::GetType()=4, SVC_Print=16,
    // SVC_UserMessage=23 - the NetSplitScreenUser-at-3 shift), so they need the
    // same `scan_game_payload` remap. But they are NOT a "portal2 engine":
    // splitscreen = 4 (handled above) and svc_UserMessage's length field is 11
    // bits, not Portal 2's 12 (SVC_UserMessage::ReadFromBuffer reads `v & 0x7FF`).
    // So the message-map remap and the 12-bit user-message width are passed as
    // two independent flags below.
    let l4d_msgmap = matches!(out.game_dir.as_str(), "left4dead" | "left4dead2");
    let remap_msgs = portal2_engine || l4d_msgmap;
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
                if length <= 0 { continue; }
                let payload_end = len_off.saturating_add(4).saturating_add(length as usize);
                if (length as usize) < (data.len() - pkt_start) && payload_end < data.len() {
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
    // CS:GO (Source 1, demo_protocol 4, net_protocol ~13xxx) protobuf-wraps its
    // net messages + SendTables, so its DEM_DATATABLES / DEM_PACKET payloads route
    // to the `csgo` decoder instead of the bit-packed path. The container walk
    // (democmdinfo, splitscreen=2, command framing) is identical to the rest of
    // the proto-4 family, so only those two arms branch.
    let is_csgo = out.game_dir.eq_ignore_ascii_case("csgo")
        || (out.demo_protocol >= 4 && (13000..14000).contains(&out.net_protocol));
    // MAX_EDICT_BITS: the bit width of edict/entity indices in svc_PacketEntities
    // (m_nMaxEntries, m_nUpdatedEntries) and the removed-entities list. Stock
    // Source is 11 (2048 edicts). GMod 13 raised the edict limit to 8192, so its
    // engine recompiles MAX_EDICT_BITS = 13 - and every entity-packet header reads
    // those two count fields two bits wider. With 11 the GMod header decodes to
    // garbage (numUpdates 1247 > maxEntries 225, length 26); with 13 it reads
    // clean (delta=1, ~8 updated entities, length == the rest of the packet).
    // Verified empirically against garrythirteen.dem (no reference parser exists).
    let edict_bits: u32 = if out.game_dir.eq_ignore_ascii_case("garrysmod") { 13 } else { 11 };
    let dbg_scan = std::env::var("DUMP_SCAN").is_ok();
    if dbg_scan {
        eprintln!("[SCAN] proto={} net={} game={} portal2_engine={} splitscreen={} csgo={}",
            out.demo_protocol, out.net_protocol, out.game_dir, portal2_engine, splitscreen_count, is_csgo);
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
                let length = le_i32(&data, offset + democmdinfo_bytes + 8);
                if length < 0 { break; }
                let payload_start = offset + democmdinfo_bytes + 12;
                let payload_end = payload_start.saturating_add(length as usize);
                if payload_end > data.len() { break; }
                // democmdinfo Split_t[0]: flags(4)+viewOrigin(12)+viewAngles(12).
                // viewAngles = the recorder's per-frame camera direction; capture
                // it as the dense FPS-camera angle source (game packets only).
                if raw_cmd == 2 && offset + 24 <= data.len() {
                    let pitch = le_f32(&data, offset + 16);
                    let yaw = le_f32(&data, offset + 20);
                    if pitch.is_finite() && yaw.is_finite() {
                        out.view_angles.push((tick, pitch, yaw));
                    }
                }
                dbg_pkts += 1;
                if is_csgo {
                    scan_csgo_payload(
                        &data[payload_start..payload_end],
                        tick,
                        data_tables.as_ref(),
                        world.as_mut(),
                        &mut last_pos, &mut origin_state, &mut last_life, &mut last_obs, &mut last_yaw, &mut last_weapon,
                        &mut out.tracks, &mut out.life_states, &mut out.observer_modes, &mut out.yaws, &mut out.weapons,
                        &mut out.weapon_classes,
                        &mut out.econ,
                        &mut csgo_string_tables,
                        &mut out.names,
                    );
                } else {
                    scan_game_payload(
                        &data[payload_start..payload_end],
                        tick,
                        out.demo_protocol,
                        remap_msgs,
                        portal2_engine,
                        edict_bits,
                        data_tables.as_ref(),
                        world.as_mut(),
                        &mut last_pos, &mut origin_state, &mut last_life, &mut last_obs, &mut last_yaw, &mut last_weapon,
                        &mut out.tracks, &mut out.life_states, &mut out.observer_modes, &mut out.yaws, &mut out.weapons,
                        &mut out.weapon_classes,
                        &mut out.econ,
                        userinfo_table_id,
                        &mut out.names,
                    );
                }
                offset = payload_end;
            }
            DEM_CONSOLECMD => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(&data, offset);
                if length < 0 { break; }
                offset = offset.saturating_add(4).saturating_add(length as usize);
            }
            DEM_USERCMD => {
                if offset + 8 > data.len() { break; }
                let length = le_i32(&data, offset + 4);
                if length < 0 { break; }
                offset = offset.saturating_add(8).saturating_add(length as usize);
            }
            DEM_DATATABLES => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(&data, offset);
                if length < 0 { break; }
                let payload_start = offset + 4;
                let payload_end = payload_start.saturating_add(length as usize).min(data.len());
                let parsed = if is_csgo {
                    // CS:GO ships SendTables as protobuf CSVCMsg_SendTable.
                    csgo::sendtable::parse(&data[payload_start..payload_end])
                } else {
                    let quirks = datatable::DataTableQuirks::for_game(&out.game_dir);
                    datatable::parse(&data[payload_start..payload_end], out.demo_protocol, quirks)
                };
                if let Some(dt) = parsed {
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
                                    eprintln!("[FLAT] {:>3} {:?} nbits={} prio={} {} {}{}",
                                        i, p.prop_type, p.bit_count, p.priority, co,
                                        p.array_parent.as_deref().map(|a| format!("{a}.")).unwrap_or_default(),
                                        p.name);
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
                let length = le_i32(&data, offset + 4);
                if length < 0 { break; }
                offset = offset.saturating_add(8).saturating_add(length as usize).min(data.len());
            }
            DEM_STRINGTABLES => {
                if offset + 4 > data.len() { break; }
                let length = le_i32(&data, offset);
                if length < 0 { break; }
                let payload_start = offset + 4;
                let payload_end = payload_start.saturating_add(length as usize).min(data.len());
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

