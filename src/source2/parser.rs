// Source 2 positions parser — ties the stages together.
//
// Walks the PBDEMS2 frames, dispatches the handful of messages needed for
// player tracks (FlattenedSerializer, ClassInfo, string tables, PacketEntities),
// decodes entity deltas, and accumulates per-pawn world positions + eye angles.
// Modelled on dotabuff/manta's parser/packet/entity flow.

use std::collections::{BTreeMap, HashMap};

use super::bitreader::BitReader;
use super::entities::{get_by_name, read_fields, Entity, FieldState};
use super::serializer::{parse_flattened, Class, Tables};
use super::snappy;
use super::stringtable::{maybe_decompress, parse_entries, StringTable};
use super::super::protobuf::Reader;
use super::super::source::events::{EventField, EventValue, GameEvent};

// EDemoCommands
const DEM_SEND_TABLES: u32 = 4;
const DEM_CLASS_INFO: u32 = 5;
const DEM_STRING_TABLES: u32 = 6;
const DEM_PACKET: u32 = 7;
const DEM_SIGNON_PACKET: u32 = 8;
const DEM_FULL_PACKET: u32 = 13;
const DEM_IS_COMPRESSED: u32 = 64;

// Inner net message ids
const SVC_SERVER_INFO: i32 = 40;
const SVC_CREATE_STRING_TABLE: i32 = 44;
const SVC_UPDATE_STRING_TABLE: i32 = 45;
const SVC_PACKET_ENTITIES: i32 = 55;
const GE_GAME_EVENT_LIST: i32 = 205; // CMsgSource1LegacyGameEventList (descriptors)
const GE_GAME_EVENT: i32 = 207; // CMsgSource1LegacyGameEvent (an instance)

/// Gameplay-significant events we keep (the stream also carries high-spam events
/// like weapon_fire / player_footstep that would bloat the timeline).
const KEEP_EVENTS: &[&str] = &[
    "player_death", "player_hurt", "player_blind", "player_spawn", "player_team",
    "player_connect", "player_connect_full", "player_disconnect",
    "bomb_planted", "bomb_defused", "bomb_exploded", "bomb_beginplant", "bomb_begindefuse",
    "bomb_dropped", "bomb_pickup", "bomb_abortplant", "bomb_abortdefuse",
    "round_start", "round_end", "round_mvp", "round_freeze_end", "round_officially_ended",
    "round_prestart", "round_poststart", "round_announce_match_start", "cs_win_panel_round",
    "hostage_rescued", "hostage_killed", "hostage_hurt", "hostage_follows",
    "smokegrenade_detonate", "flashbang_detonate", "hegrenade_detonate", "molotov_detonate",
    "inferno_startburn", "decoy_started", "item_pickup", "weapon_zoom",
];

const MAX_COORD: f32 = 16384.0;
// Fallback cell width when the m_vecX field's high value isn't available.
// CS2/Deadlock use 512 (offset high 1024); Dota uses 128 (offset high 256).
const DEFAULT_CELL_WIDTH: f32 = 512.0;

fn cell_coord(cell: u64, offset: f32, cell_width: f32) -> f32 {
    (cell as f32) * cell_width - MAX_COORD + offset
}

#[derive(Default)]
#[allow(private_interfaces)] // GameEvent is a crate-internal type (source::events)
pub struct Source2Tracks {
    pub tracks: BTreeMap<i32, Vec<(i32, f32, f32, f32)>>, // pawn idx -> (tick,x,y,z)
    pub yaws: BTreeMap<i32, Vec<(i32, f32, f32)>>,         // pawn idx -> (tick,yaw,pitch)
    pub names: BTreeMap<i32, String>,                      // pawn idx -> player name
    pub life_states: BTreeMap<i32, Vec<(i32, u8)>>,        // pawn idx -> (tick, m_lifeState) transitions
    pub weapons: BTreeMap<i32, Vec<(i32, i32)>>,           // pawn idx -> (tick, weapon class_id) on change
    pub weapon_names: HashMap<i32, String>,               // weapon class_id -> short display name
    pub buttons: BTreeMap<i32, Vec<(i32, u32)>>,          // pawn idx -> (tick, normalized button mask) on change
    pub rounds: Vec<(i32, i32)>,                           // (tick, total rounds played) transitions
    pub events: Vec<GameEvent>,                            // decoded gameplay events (kills, bomb, rounds…)
    pub econ: BTreeMap<i32, PlayerEcon>,                   // controller idx -> latest economy/score
    pub econ_by_pawn: BTreeMap<i32, PlayerEcon>,           // pawn idx -> economy (for the track sidebar)
    pub map_name: String,
    pub pe_ok: usize,
    pub pe_fail: usize,
}

/// Latest per-player economy + scoreboard snapshot (controller fields).
#[derive(Default, Clone)]
pub struct PlayerEcon {
    pub name: String,
    pub money: i32,
    pub kills: i32,
    pub deaths: i32,
    pub assists: i32,
    pub score: i32,
    pub team: i32,
}

struct Parser {
    tables: Option<Tables>,
    class_id_size: u32,
    max_classes: i32,
    string_tables: Vec<StringTable>,
    name_to_table: HashMap<String, usize>,
    class_baselines: HashMap<i32, Vec<u8>>,
    entities: HashMap<i32, Entity>,
    tick: i32,
    // Name resolution (resolved at finalize): controller idx -> player name, plus
    // two pawn -> controller links. `pawn_ctrl_via_pawn` (controller's current
    // m_hPlayerPawn) covers live pawns; `pawn_ctrl_self` (the pawn's own
    // m_hController) covers stale pawns the controller no longer points at.
    controller_names: HashMap<i32, String>,
    pawn_ctrl_via_pawn: HashMap<i32, i32>,
    pawn_ctrl_self: HashMap<i32, i32>,
    // Dota tracks the assigned HERO unit, not the (frozen, spawn-parked)
    // CDOTAPlayerPawn. `hero_ctrl` maps a controller's m_hAssignedHero entity
    // index -> controller idx, so we position-track heroes and name them.
    is_dota: bool,
    hero_ctrl: HashMap<i32, i32>,
    // cell width per serializer, derived once from m_vecX's high value (high/2).
    cell_width: HashMap<usize, f32>,
    last_life: HashMap<i32, u8>,
    last_weapon: HashMap<i32, i32>, // pawn idx -> last active-weapon class_id (change detection)
    last_buttons: HashMap<i32, u32>, // pawn idx -> last normalized button mask (change detection)
    // CInferno (molotov/incendiary fire) entity idx -> owner pawn idx. The
    // inferno_startburn event names only the fire entity, which is created a beat
    // AFTER the event fires, so we accumulate owners as infernos appear and
    // backfill the events at finalize.
    inferno_owners: HashMap<i32, i32>,
    last_round: i32,
    event_descriptors: HashMap<i32, (String, Vec<String>)>, // eventid -> (name, key names)
    out: Source2Tracks,
}

pub fn parse(data: &[u8]) -> Option<Source2Tracks> {
    if !super::is_source2(data) || data.len() < 16 {
        return None;
    }
    let mut p = Parser {
        tables: None,
        class_id_size: 0,
        max_classes: 0,
        string_tables: Vec::new(),
        name_to_table: HashMap::new(),
        class_baselines: HashMap::new(),
        entities: HashMap::new(),
        tick: 0,
        controller_names: HashMap::new(),
        pawn_ctrl_via_pawn: HashMap::new(),
        pawn_ctrl_self: HashMap::new(),
        is_dota: false,
        hero_ctrl: HashMap::new(),
        cell_width: HashMap::new(),
        last_life: HashMap::new(),
        last_weapon: HashMap::new(),
        last_buttons: HashMap::new(),
        inferno_owners: HashMap::new(),
        last_round: -1,
        event_descriptors: HashMap::new(),
        out: Source2Tracks::default(),
    };

    // Skip 8-byte magic + two i32 offsets.
    let mut r = Reader::new(&data[16..]);
    loop {
        let cmd = match r.read_varint() {
            Ok(v) => v as u32,
            Err(_) => break,
        };
        let tick = match r.read_varint() {
            Ok(v) => v as u32,
            Err(_) => break,
        };
        let size = match r.read_varint() {
            Ok(v) => v as usize,
            Err(_) => break,
        };
        let raw = match r.read_bytes(size) {
            Ok(b) => b,
            Err(_) => break,
        };
        let compressed = cmd & DEM_IS_COMPRESSED != 0;
        let kind = cmd & !DEM_IS_COMPRESSED;
        if kind == 0 {
            break; // DEM_Stop
        }
        let body = if compressed {
            match snappy::decompress(raw) {
                Some(b) => b,
                None => continue,
            }
        } else {
            raw.to_vec()
        };
        // -1 tick (pre-game) shows up as u32 max.
        p.tick = if tick == u32::MAX { 0 } else { tick as i32 };
        p.handle_frame(kind, &body);
    }

    p.finalize();
    Some(p.out)
}

impl Parser {
    fn handle_frame(&mut self, kind: u32, body: &[u8]) {
        match kind {
            DEM_SEND_TABLES => self.on_send_tables(body),
            DEM_CLASS_INFO => self.on_class_info(body),
            DEM_STRING_TABLES => self.on_demo_string_tables(body),
            DEM_PACKET | DEM_SIGNON_PACKET => self.on_packet(body),
            DEM_FULL_PACKET => self.on_full_packet(body),
            _ => {}
        }
    }

    /// CDemoStringTables (periodic full snapshot of the string tables, and the
    /// `string_table` half of a FullPacket). This is where CS2 actually delivers
    /// `instancebaseline` (per-class entity baselines) and `userinfo` (player
    /// names) — the in-packet Create/Update messages are empty.
    fn on_demo_string_tables(&mut self, body: &[u8]) {
        let mut r = Reader::new(body);
        while let Ok(Some(f)) = r.next_field() {
            if f.number == 1 {
                if let Some(tb) = f.value.as_bytes() {
                    self.parse_demo_table(tb);
                }
            }
        }
    }

    fn parse_demo_table(&mut self, body: &[u8]) {
        let mut name = String::new();
        let mut items: Vec<(String, Vec<u8>)> = Vec::new();
        let mut r = Reader::new(body);
        while let Ok(Some(f)) = r.next_field() {
            match f.number {
                1 => name = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
                2 => {
                    if let Some(ib) = f.value.as_bytes() {
                        // items_t { str = 1, data = 2 }
                        let mut s = String::new();
                        let mut d: Vec<u8> = Vec::new();
                        let mut ir = Reader::new(ib);
                        while let Ok(Some(ff)) = ir.next_field() {
                            match ff.number {
                                1 => s = ff.value.as_str().map(|x| x.into_owned()).unwrap_or_default(),
                                2 => d = ff.value.as_bytes().map(|x| x.to_vec()).unwrap_or_default(),
                                _ => {}
                            }
                        }
                        items.push((s, d));
                    }
                }
                _ => {}
            }
        }

        match name.as_str() {
            "instancebaseline" => {
                for (s, d) in &items {
                    if let Ok(cid) = s.parse::<i32>() {
                        self.class_baselines.insert(cid, d.clone());
                    }
                }
            }
            "userinfo" => {
                // Item N => player slot N => controller entity N+1. data is a
                // CMsgPlayerInfo; field 1 is the player name.
                for (i, (_s, d)) in items.iter().enumerate() {
                    if d.is_empty() {
                        continue;
                    }
                    let mut pr = Reader::new(d);
                    while let Ok(Some(ff)) = pr.next_field() {
                        if ff.number == 1 {
                            if let Some(nm) = ff.value.as_str() {
                                let nm = nm.into_owned();
                                if !nm.is_empty() {
                                    self.controller_names.insert(i as i32 + 1, nm);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn on_send_tables(&mut self, body: &[u8]) {
        // CDemoSendTables { data = 1 }; data = varint(len) + CSVCMsg_FlattenedSerializer.
        let mut r = Reader::new(body);
        let mut data: Option<&[u8]> = None;
        while let Ok(Some(f)) = r.next_field() {
            if f.number == 1 {
                data = f.value.as_bytes();
            }
        }
        if let Some(d) = data {
            let mut rr = Reader::new(d);
            if let Ok(len) = rr.read_varint() {
                if let Ok(msg) = rr.read_bytes(len as usize) {
                    if let Some(t) = parse_flattened(msg) {
                        self.tables = Some(t);
                    }
                }
            }
        }
    }

    fn on_class_info(&mut self, body: &[u8]) {
        let tables = match self.tables.as_mut() {
            Some(t) => t,
            None => return,
        };
        let mut r = Reader::new(body);
        let mut count = 0i32;
        while let Ok(Some(f)) = r.next_field() {
            if f.number == 1 {
                if let Some(cb) = f.value.as_bytes() {
                    let (class_id, network_name) = parse_class_t(cb);
                    if network_name.starts_with("CDOTA") {
                        self.is_dota = true;
                    }
                    let ser_idx = tables.by_name.get(&network_name).copied();
                    tables.classes_by_id.insert(
                        class_id,
                        Class { class_id, name: network_name, serializer_idx: ser_idx },
                    );
                    count += 1;
                }
            }
        }
        if self.max_classes == 0 {
            self.max_classes = count;
        }
        // classIdSize = floor(log2(maxClasses)) + 1.
        let n = self.max_classes.max(1) as f64;
        self.class_id_size = (n.log2().floor() as u32) + 1;
    }

    fn on_full_packet(&mut self, body: &[u8]) {
        // CDemoFullPacket { string_table = 1 (CDemoStringTables), packet = 2 }.
        let mut r = Reader::new(body);
        while let Ok(Some(f)) = r.next_field() {
            match f.number {
                1 => {
                    if let Some(st) = f.value.as_bytes() {
                        self.on_demo_string_tables(st);
                    }
                }
                2 => {
                    if let Some(pkt) = f.value.as_bytes() {
                        self.on_packet(pkt);
                    }
                }
                _ => {}
            }
        }
    }

    fn on_packet(&mut self, body: &[u8]) {
        // CDemoPacket { data = 3 }.
        let mut r = Reader::new(body);
        let mut data: Option<&[u8]> = None;
        while let Ok(Some(f)) = r.next_field() {
            if f.number == 3 {
                data = f.value.as_bytes();
            }
        }
        let data = match data {
            Some(d) => d,
            None => return,
        };

        // Inner messages: ubitvar(type) + varint(size) + bytes. Collect then
        // sort so string tables are applied before PacketEntities.
        let mut msgs: Vec<(i32, Vec<u8>)> = Vec::new();
        let mut br = BitReader::new(data);
        while br.rem_bytes() > 0 {
            let t = br.read_ubit_var() as i32;
            let size = br.read_var_u32() as usize;
            let buf = br.read_bytes(size);
            if buf.len() < size {
                break;
            }
            msgs.push((t, buf));
        }
        msgs.sort_by_key(|(t, _)| msg_priority(*t));

        for (t, buf) in &msgs {
            match *t {
                SVC_SERVER_INFO => self.on_server_info(buf),
                SVC_CREATE_STRING_TABLE => self.on_create_string_table(buf),
                SVC_UPDATE_STRING_TABLE => self.on_update_string_table(buf),
                SVC_PACKET_ENTITIES => self.on_packet_entities(buf),
                GE_GAME_EVENT_LIST => self.on_game_event_list(buf),
                GE_GAME_EVENT => self.on_game_event(buf),
                _ => {}
            }
        }
    }

    /// CMsgSource1LegacyGameEventList — descriptors mapping eventid -> name + the
    /// ordered key names. Game events themselves carry only values, in this order.
    fn on_game_event_list(&mut self, body: &[u8]) {
        let mut r = Reader::new(body);
        while let Ok(Some(f)) = r.next_field() {
            if f.number == 1 {
                if let Some(d) = f.value.as_bytes() {
                    let mut eventid = 0i32;
                    let mut name = String::new();
                    let mut keys: Vec<String> = Vec::new();
                    let mut dr = Reader::new(d);
                    while let Ok(Some(ff)) = dr.next_field() {
                        match ff.number {
                            1 => eventid = ff.value.as_i32().unwrap_or(0),
                            2 => name = ff.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
                            3 => {
                                // key_t { type=1, name=2 }
                                if let Some(kb) = ff.value.as_bytes() {
                                    let mut kr = Reader::new(kb);
                                    let mut kn = String::new();
                                    while let Ok(Some(kf)) = kr.next_field() {
                                        if kf.number == 2 {
                                            kn = kf.value.as_str().map(|s| s.into_owned()).unwrap_or_default();
                                        }
                                    }
                                    keys.push(kn);
                                }
                            }
                            _ => {}
                        }
                    }
                    self.event_descriptors.insert(eventid, (name, keys));
                }
            }
        }
    }

    /// CMsgSource1LegacyGameEvent — one event; values are zipped against the
    /// descriptor's key names. Kept only if gameplay-significant (KEEP_EVENTS).
    fn on_game_event(&mut self, body: &[u8]) {
        let mut eventid = 0i32;
        let mut event_name = String::new();
        let mut values: Vec<EventValue> = Vec::new();
        let mut r = Reader::new(body);
        while let Ok(Some(f)) = r.next_field() {
            match f.number {
                1 => event_name = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
                2 => eventid = f.value.as_i32().unwrap_or(0),
                3 => {
                    if let Some(kb) = f.value.as_bytes() {
                        values.push(decode_event_value(kb));
                    }
                }
                _ => {}
            }
        }

        let (name, keys) = match self.event_descriptors.get(&eventid) {
            Some((n, k)) => (n.clone(), k.clone()),
            None => (event_name, Vec::new()),
        };
        if !KEEP_EVENTS.contains(&name.as_str()) {
            return;
        }
        let mut fields: Vec<EventField> = keys
            .into_iter()
            .zip(values.into_iter())
            .map(|(name, value)| EventField { name, value })
            .collect();
        // Rewrite player references so the viewer can attribute kills. CS2 game
        // events carry the player as a *slot/userid* (e.g. attacker=1) PLUS a
        // `*_pawn` entity handle (attacker_pawn). The viewer keys names by pawn
        // entity index, so the raw slot resolves to nobody and "who killed whom"
        // comes up blank. We replace userid/attacker/assister with the pawn index
        // decoded from their `*_pawn` handle (low 14 bits), which is exactly what
        // ENTITY_NAMES / ENTITY_TRACKS are keyed by. An invalid handle (world /
        // suicide / no assister) maps to 0 so the viewer treats it as no attacker.
        for base in ["userid", "attacker", "assister"] {
            let pawn_handle = fields.iter()
                .find(|f| f.name == format!("{}_pawn", base))
                .and_then(|f| match &f.value {
                    EventValue::Int(v) => Some(*v as u32),
                    _ => None,
                });
            if let Some(h) = pawn_handle {
                let idx = (h & 0x3FFF) as i32;
                let resolved = if idx == 0x3FFF { 0 } else { idx };
                // Overwrite the slot-based value if present; otherwise CREATE it.
                // Some events (decoy_started, inferno_startburn, …) ship only the
                // `*_pawn` handle with no plain `userid`, so the thrower would
                // resolve to nobody unless we add the field from the handle.
                match fields.iter_mut().find(|f| f.name == base) {
                    Some(f) => f.value = EventValue::Int(resolved),
                    None => fields.push(EventField { name: base.to_string(), value: EventValue::Int(resolved) }),
                }
            }
        }
        self.out.events.push(GameEvent { event: name, tick: self.tick, fields });
    }

    fn on_server_info(&mut self, body: &[u8]) {
        let mut r = Reader::new(body);
        while let Ok(Some(f)) = r.next_field() {
            match f.number {
                11 => {
                    if let Some(mc) = f.value.as_i32() {
                        self.max_classes = mc;
                        let n = mc.max(1) as f64;
                        self.class_id_size = (n.log2().floor() as u32) + 1;
                    }
                }
                15 => {
                    if let Some(s) = f.value.as_str() {
                        if self.out.map_name.is_empty() {
                            self.out.map_name = s.into_owned();
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn on_create_string_table(&mut self, body: &[u8]) {
        let mut name = String::new();
        let mut num_entries = 0i32;
        let mut user_data_fixed = false;
        let mut user_data_size_bits = 0i32;
        let mut flags = 0i32;
        let mut string_data: Vec<u8> = Vec::new();
        let mut data_compressed = false;
        let mut varint_bitcounts = false;

        let mut r = Reader::new(body);
        while let Ok(Some(f)) = r.next_field() {
            match f.number {
                1 => name = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
                2 => num_entries = f.value.as_i32().unwrap_or(0),
                3 => user_data_fixed = f.value.as_bool().unwrap_or(false),
                5 => user_data_size_bits = f.value.as_i32().unwrap_or(0),
                6 => flags = f.value.as_i32().unwrap_or(0),
                7 => string_data = f.value.as_bytes().map(|b| b.to_vec()).unwrap_or_default(),
                9 => data_compressed = f.value.as_bool().unwrap_or(false),
                10 => varint_bitcounts = f.value.as_bool().unwrap_or(false),
                _ => {}
            }
        }

        let buf = match maybe_decompress(&string_data, data_compressed) {
            Some(b) => b,
            None => return,
        };
        let items = parse_entries(&buf, num_entries, user_data_fixed, user_data_size_bits, flags, varint_bitcounts);

        let idx = self.string_tables.len();
        let mut table = StringTable {
            name: name.clone(),
            user_data_fixed_size: user_data_fixed,
            user_data_size_bits,
            flags,
            varint_bitcounts,
            items: HashMap::new(),
        };
        for it in items {
            table.items.insert(it.index, (it.key, it.value));
        }
        self.name_to_table.insert(name.clone(), idx);
        self.string_tables.push(table);
        if name == "instancebaseline" {
            self.rebuild_baselines(idx);
        }
    }

    fn on_update_string_table(&mut self, body: &[u8]) {
        let mut table_id = -1i32;
        let mut num_changed = 0i32;
        let mut string_data: Vec<u8> = Vec::new();
        let mut r = Reader::new(body);
        while let Ok(Some(f)) = r.next_field() {
            match f.number {
                1 => table_id = f.value.as_i32().unwrap_or(-1),
                2 => num_changed = f.value.as_i32().unwrap_or(0),
                3 => string_data = f.value.as_bytes().map(|b| b.to_vec()).unwrap_or_default(),
                _ => {}
            }
        }
        let idx = table_id as usize;
        let (fixed, bits, flags, varint, name) = match self.string_tables.get(idx) {
            Some(t) => (t.user_data_fixed_size, t.user_data_size_bits, t.flags, t.varint_bitcounts, t.name.clone()),
            None => return,
        };
        let items = parse_entries(&string_data, num_changed, fixed, bits, flags, varint);
        if let Some(t) = self.string_tables.get_mut(idx) {
            for it in items {
                let entry = t.items.entry(it.index).or_insert_with(|| (String::new(), Vec::new()));
                if !it.key.is_empty() {
                    entry.0 = it.key;
                }
                if !it.value.is_empty() {
                    entry.1 = it.value;
                }
            }
        }
        if name == "instancebaseline" {
            self.rebuild_baselines(idx);
        }
    }

    fn rebuild_baselines(&mut self, table_idx: usize) {
        if let Some(t) = self.string_tables.get(table_idx) {
            for (_, (key, value)) in t.items.iter() {
                if let Ok(class_id) = key.parse::<i32>() {
                    self.class_baselines.insert(class_id, value.clone());
                }
            }
        }
    }

    fn on_packet_entities(&mut self, body: &[u8]) {
        let tables = match self.tables.as_ref() {
            Some(t) => t,
            None => return,
        };
        if self.class_id_size == 0 {
            return;
        }

        let mut updated_entries = 0i32;
        let mut entity_data: &[u8] = &[];
        let mut is_delta = false;
        let mut has_pvs_vis_bits = 0u32;
        let mut r = Reader::new(body);
        while let Ok(Some(f)) = r.next_field() {
            match f.number {
                2 => updated_entries = f.value.as_i32().unwrap_or(0),
                3 => is_delta = f.value.as_bool().unwrap_or(false),
                7 => entity_data = f.value.as_bytes().unwrap_or(&[]),
                16 => has_pvs_vis_bits = f.value.as_u32().unwrap_or(0),
                _ => {}
            }
        }
        let _ = is_delta;
        if entity_data.is_empty() {
            return;
        }

        let mut br = BitReader::new(entity_data);
        let mut index: i32 = -1;
        let mut ok = true;

        for _ in 0..updated_entries {
            index += br.read_ubit_var() as i32 + 1;
            let cmd = br.read_bits(2);
            if cmd & 0x01 == 0 {
                if cmd & 0x02 != 0 {
                    // create + enter
                    let class_id = br.read_bits(self.class_id_size) as i32;
                    let serial = br.read_bits(17) as i32;
                    let _ = br.read_var_u32();

                    let ser_idx = match tables.classes_by_id.get(&class_id).and_then(|c| c.serializer_idx) {
                        Some(s) => s,
                        None => { ok = false; break; }
                    };
                    let mut state = FieldState::new();
                    if let Some(baseline) = self.class_baselines.get(&class_id) {
                        let mut bl = BitReader::new(baseline);
                        read_fields(tables, ser_idx, &mut bl, &mut state);
                    }
                    if !read_fields(tables, ser_idx, &mut br, &mut state) {
                        ok = false;
                        break;
                    }
                    self.entities.insert(index, Entity {
                        index,
                        serial,
                        class_id,
                        serializer_idx: ser_idx,
                        state,
                        active: true,
                    });
                } else {
                    // update
                    if has_pvs_vis_bits != 0 {
                        // 2 PVS bits; 0x01 set => entity not (re)transmitted here.
                        if br.read_bits(2) & 0x01 == 1 {
                            continue;
                        }
                    }
                    let ser_idx = match self.entities.get(&index) {
                        Some(e) => e.serializer_idx,
                        None => { ok = false; break; }
                    };
                    let mut state = std::mem::replace(
                        &mut self.entities.get_mut(&index).unwrap().state,
                        FieldState::new(),
                    );
                    let res = read_fields(tables, ser_idx, &mut br, &mut state);
                    self.entities.get_mut(&index).unwrap().state = state;
                    if !res {
                        ok = false;
                        break;
                    }
                }
            } else if cmd & 0x02 != 0 {
                // leave + delete
                self.entities.remove(&index);
            }
            // (leave-only just marks inactive; we keep the last state for sampling)
        }

        if ok {
            self.out.pe_ok += 1;
        } else {
            self.out.pe_fail += 1;
        }
        // Sample regardless of `ok`: the entity loop runs in ascending index order
        // and applies each delta as it goes, so every entity processed *before* a
        // break already holds correct this-tick state. Player pawns/heroes are
        // low-index and decode before the high-index transient entity that
        // occasionally can't be resolved (an un-created entity referenced by a
        // delta — a PVS/baseline edge case), so this recovers their positions for
        // the ~5% of ticks that would otherwise be dropped. A read-fields desync
        // only corrupts the single failing (non-player) entity, never the players
        // already updated earlier in the loop.
        self.sample();
    }

    /// After a successful PacketEntities, record positions for player pawns and
    /// names for player controllers.
    fn sample(&mut self) {
        let tables = match self.tables.as_ref() {
            Some(t) => t,
            None => return,
        };
        let tick = self.tick;
        for (idx, e) in self.entities.iter() {
            let idx = *idx;
            let class_name = tables
                .classes_by_id
                .get(&e.class_id)
                .map(|c| c.name.as_str())
                .unwrap_or("");

            // The tracked "avatar" is the player pawn in CS2/Deadlock, but the
            // assigned HERO unit in Dota (the CDOTAPlayerPawn is parked at spawn).
            let is_avatar = if self.is_dota {
                self.hero_ctrl.contains_key(&idx)
            } else {
                class_name.contains("PlayerPawn")
            };

            if is_avatar {
                // Cell width is a per-serializer constant; derive once (high/2).
                let cw = *self.cell_width.entry(e.serializer_idx).or_insert_with(|| {
                    tables.field_high(e.serializer_idx, "CBodyComponent.m_vecX")
                        .map(|h| h / 2.0)
                        .unwrap_or(DEFAULT_CELL_WIDTH)
                });
                if let Some(pos) = pawn_world_pos(tables, e, cw) {
                    self.out.tracks.entry(idx).or_default().push((tick, pos[0], pos[1], pos[2]));
                    // Eye angles for remote players; fall back to the client camera
                    // angles for the local (recording) player, whose eye angles the
                    // server doesn't echo back to them (so they'd otherwise have none).
                    if let Some(ang) = get_by_name(tables, e, "m_angEyeAngles")
                        .or_else(|| get_by_name(tables, e, "m_angClientCamera"))
                        .and_then(|v| v.as_vec3())
                    {
                        self.out.yaws.entry(idx).or_default().push((tick, ang[1], ang[0]));
                    }
                }
                // Fallback name link: a stale pawn keeps its own m_hController.
                if let Some(h) = get_by_name(tables, e, "m_hController").and_then(|v| v.as_u64()) {
                    let c = (h & 0x3FFF) as i32;
                    if c != 0 { self.pawn_ctrl_self.insert(idx, c); }
                }
                // Death tracking: record m_lifeState transitions (0=alive, 2=dead).
                if let Some(ls) = get_by_name(tables, e, "m_lifeState").and_then(|v| v.as_u64()) {
                    let ls = ls as u8;
                    if self.last_life.get(&idx) != Some(&ls) {
                        self.last_life.insert(idx, ls);
                        self.out.life_states.entry(idx).or_default().push((tick, ls));
                    }
                }
                // Active-weapon tracking. m_pWeaponServices.m_hActiveWeapon is a
                // CHandle on the CS2 player pawn; its low 14 bits index the weapon
                // entity, whose network class is the weapon's identity. We key the
                // stream on the weapon's class_id (stable for the whole demo,
                // distinct per weapon) rather than the recycled entity index, and
                // emit only on change — matching the Source 1 weapon stream the
                // viewer already renders.
                //
                // This resolves for CS2 (CCSPlayerPawn). Deadlock's CCitadelPlayerPawn
                // and Dota's hero units have no held weapon — they network an ability
                // system (m_vecAbilities / m_hSelectedAbility) instead — so the lookup
                // simply finds nothing there, by design. The bare m_hActiveWeapon
                // fallback covers any other Source 2 pawn that networks it directly.
                if let Some(h) = get_by_name(tables, e, "m_pWeaponServices.m_hActiveWeapon")
                    .or_else(|| get_by_name(tables, e, "m_hActiveWeapon"))
                    .and_then(|v| v.as_u64())
                {
                    let widx = (h & 0x3FFF) as i32;
                    // 0 = worldspawn, 0x3FFF = the null-handle sentinel: no weapon.
                    let wclass = if widx != 0 && widx != 0x3FFF {
                        self.entities.get(&widx).map(|w| w.class_id)
                    } else {
                        None
                    };
                    if let Some(cid) = wclass {
                        if self.last_weapon.get(&idx) != Some(&cid) {
                            self.last_weapon.insert(idx, cid);
                            self.out.weapons.entry(idx).or_default().push((tick, cid));
                            self.out.weapon_names.entry(cid).or_insert_with(|| {
                                let cn = tables.classes_by_id.get(&cid).map(|c| c.name.as_str()).unwrap_or("");
                                weapon_display_name(cn)
                            });
                        }
                    }
                }

                // Real input state. CS2 has no usercmd stream in the demo, but the
                // pawn's movement services networks the held-button mask
                // (m_nButtonDownMaskPrev) plus explicit jump/duck booleans — so we
                // can surface ACTUAL W/A/S/D/attack/jump/duck, not motion guesses.
                //
                // CS2's directional + attack bits sit at the same positions as the
                // classic engine IN_ flags (attack=1, forward=8, back=16,
                // moveleft=512, moveright=1024), verified by correlating each bit
                // against real velocity direction. We pass those through into the
                // viewer's existing mask layout. Jump/duck are NOT reliable in the
                // mask on CS2 (those bits barely toggled in testing), so we take
                // them from the dedicated booleans the movement services expose.
                if let Some(raw) = get_by_name(tables, e, "m_pMovementServices.m_nButtonDownMaskPrev")
                    .and_then(|v| v.as_u64())
                {
                    // CS2's mask uses the classic engine IN_ bit layout — verified
                    // two ways: directional bits by velocity correlation (bit3
                    // forward, bit4 back, 512/1024 strafes), and jump/duck by
                    // matching the mask bits against the movement services'
                    // m_bDesiresDuck boolean (mask&4 == bDesiresDuck exactly across
                    // demos). So we keep just the seven bits the viewer renders:
                    //   attack=1 jump=2 duck=4 forward=8 back=16 moveleft=512 moveright=1024
                    const KEEP: u64 = 1 | 2 | 4 | 8 | 16 | 512 | 1024;
                    let mask = (raw & KEEP) as u32;
                    if self.last_buttons.get(&idx) != Some(&mask) {
                        self.last_buttons.insert(idx, mask);
                        self.out.buttons.entry(idx).or_default().push((tick, mask));
                    }
                }
            }
            if class_name.contains("PlayerController") {
                if let Some(name) = get_by_name(tables, e, "m_iszPlayerName").and_then(|v| v.as_str().map(|s| s.to_string())) {
                    if !name.is_empty() {
                        self.controller_names.insert(idx, name);
                    }
                }
                // Economy + scoreboard snapshot (latest wins).
                let geti = |n: &str| get_by_name(tables, e, n).and_then(|v| v.as_u64()).map(|x| x as i32);
                let econ = self.out.econ.entry(idx).or_default();
                if let Some(m) = geti("m_pInGameMoneyServices.m_iAccount") { econ.money = m; }
                if let Some(k) = geti("m_pActionTrackingServices.m_iKills") { econ.kills = k; }
                if let Some(d) = geti("m_pActionTrackingServices.m_iDeaths") { econ.deaths = d; }
                if let Some(a) = geti("m_pActionTrackingServices.m_iAssists") { econ.assists = a; }
                if let Some(s) = geti("m_iScore") { econ.score = s; }
                if let Some(t) = geti("m_iTeamNum") { econ.team = t; }
                // Primary name link: controller's current pawn.
                if let Some(h) = get_by_name(tables, e, "m_hPlayerPawn")
                    .or_else(|| get_by_name(tables, e, "m_hPawn"))
                    .and_then(|v| v.as_u64())
                {
                    let p = (h & 0x3FFF) as i32;
                    if p != 0 { self.pawn_ctrl_via_pawn.insert(p, idx); }
                }
                // Dota: the controller's assigned hero is the unit we actually
                // position-track. Handle low 14 bits = entity index; 0x3FFF = null.
                if let Some(h) = get_by_name(tables, e, "m_hAssignedHero").and_then(|v| v.as_u64()) {
                    let hero = (h & 0x3FFF) as i32;
                    if hero != 0 && hero != 0x3FFF { self.hero_ctrl.insert(hero, idx); }
                }
            } else if class_name.contains("GameRulesProxy") {
                // Round tracking: m_totalRoundsPlayed lives on the embedded rules.
                if let Some(r) = get_by_name(tables, e, "m_pGameRules.m_totalRoundsPlayed")
                    .and_then(|v| v.as_u64())
                {
                    let r = r as i32;
                    if r != self.last_round {
                        self.last_round = r;
                        self.out.rounds.push((tick, r));
                    }
                }
            } else if class_name == "CInferno" {
                // Record the fire's owner pawn so inferno_startburn (which names
                // only this entity, created just after the event) can be
                // attributed to its thrower at finalize. Handle low 14 bits = idx.
                if let Some(h) = get_by_name(tables, e, "m_hOwnerEntity").and_then(|v| v.as_u64()) {
                    let owner = (h & 0x3FFF) as i32;
                    if owner != 0 && owner != 0x3FFF {
                        self.inferno_owners.insert(idx, owner);
                    }
                }
            }
        }
    }

    fn finalize(&mut self) {
        // Attribute molotov/incendiary fires: the inferno_startburn event names
        // only its CInferno entity (no thrower), so set `userid` to that fire's
        // owner pawn (collected as the inferno entities appeared).
        for ev in self.out.events.iter_mut() {
            if ev.event != "inferno_startburn" {
                continue;
            }
            let entityid = ev.fields.iter()
                .find(|f| f.name == "entityid")
                .and_then(|f| match &f.value { EventValue::Int(n) => Some(*n), _ => None });
            if let Some(owner) = entityid.and_then(|id| self.inferno_owners.get(&id).copied()) {
                match ev.fields.iter_mut().find(|f| f.name == "userid") {
                    Some(f) => f.value = EventValue::Int(owner),
                    None => ev.fields.push(EventField { name: "userid".to_string(), value: EventValue::Int(owner) }),
                }
            }
        }
        // Resolve each tracked pawn's name: prefer the controller's current-pawn
        // link, fall back to the pawn's own m_hController for stale pawns.
        for &pawn in self.out.tracks.keys() {
            let ctrl = self.pawn_ctrl_via_pawn.get(&pawn)
                .or_else(|| self.pawn_ctrl_self.get(&pawn))
                .or_else(|| self.hero_ctrl.get(&pawn));
            if let Some(name) = ctrl.and_then(|c| self.controller_names.get(c)) {
                self.out.names.insert(pawn, name.clone());
            }
        }
        // Attach names to economy snapshots, drop empty/non-player controllers.
        for (idx, econ) in self.out.econ.iter_mut() {
            if let Some(n) = self.controller_names.get(idx) {
                econ.name = n.clone();
            }
        }
        self.out.econ.retain(|_, e| !e.name.is_empty());
        // Map each tracked pawn to its controller's economy snapshot.
        for &pawn in self.out.tracks.keys() {
            let ctrl = self.pawn_ctrl_via_pawn.get(&pawn)
                .or_else(|| self.pawn_ctrl_self.get(&pawn))
                .or_else(|| self.hero_ctrl.get(&pawn));
            if let Some(econ) = ctrl.and_then(|c| self.out.econ.get(c)) {
                self.out.econ_by_pawn.insert(pawn, econ.clone());
            }
        }
        // Drop pawns that never produced a real position sample.
        self.out.tracks.retain(|_, v| !v.is_empty());
    }
}

/// Turn a Source 2 weapon entity's network class name into a short display
/// label, mirroring the Source 1 path's "CTFRocketLauncher" -> "rocketlauncher"
/// trim. CS2 weapons are their own classes (CAK47, CWeaponAWP, CDEagle, CC4…);
/// Deadlock uses CWeapon*/CCitadel*. Strip the leading `C` and a redundant
/// `Weapon`/`Citadel` prefix, then lowercase. Falls back to the raw class.
fn weapon_display_name(class: &str) -> String {
    if class.is_empty() {
        return String::new();
    }
    let s = class.strip_prefix('C').unwrap_or(class);
    let s = s.strip_prefix("Weapon").or_else(|| s.strip_prefix("Citadel")).unwrap_or(s);
    let s = s.trim_start_matches('_');
    if s.is_empty() { class.to_lowercase() } else { s.to_lowercase() }
}

fn pawn_world_pos(tables: &Tables, e: &Entity, cell_width: f32) -> Option<[f32; 3]> {
    let cx = get_by_name(tables, e, "CBodyComponent.m_cellX")?.as_u64()?;
    let cy = get_by_name(tables, e, "CBodyComponent.m_cellY")?.as_u64()?;
    let cz = get_by_name(tables, e, "CBodyComponent.m_cellZ")?.as_u64()?;
    let vx = get_by_name(tables, e, "CBodyComponent.m_vecX")?.as_f32()?;
    let vy = get_by_name(tables, e, "CBodyComponent.m_vecY")?.as_f32()?;
    let vz = get_by_name(tables, e, "CBodyComponent.m_vecZ")?.as_f32()?;
    Some([cell_coord(cx, vx, cell_width), cell_coord(cy, vy, cell_width), cell_coord(cz, vz, cell_width)])
}

fn msg_priority(t: i32) -> i32 {
    match t {
        4 /* net_Tick */ | SVC_CREATE_STRING_TABLE | SVC_UPDATE_STRING_TABLE => -10,
        SVC_PACKET_ENTITIES => 5,
        GE_GAME_EVENT => 10, // events reference entity state, so run last
        _ => 0,
    }
}

/// Decode one CMsgSource1LegacyGameEvent.key_t into an EventValue (only the
/// populated val_* field is present on the wire).
fn decode_event_value(body: &[u8]) -> EventValue {
    let mut r = Reader::new(body);
    let mut v = EventValue::Null;
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            2 => v = EventValue::Str(f.value.as_str().map(|s| s.into_owned()).unwrap_or_default()),
            3 => v = EventValue::Float(f.value.as_f32().unwrap_or(0.0)),
            4 | 5 | 6 => v = EventValue::Int(f.value.as_i32().unwrap_or(0)),
            7 => v = EventValue::Bool(f.value.as_bool().unwrap_or(false)),
            8 => v = EventValue::Int(f.value.as_u64().unwrap_or(0) as i32),
            _ => {}
        }
    }
    v
}

fn parse_class_t(body: &[u8]) -> (i32, String) {
    let mut class_id = 0i32;
    let mut name = String::new();
    let mut r = Reader::new(body);
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            1 => class_id = f.value.as_i32().unwrap_or(0),
            2 => name = f.value.as_str().map(|s| s.into_owned()).unwrap_or_default(),
            _ => {}
        }
    }
    (class_id, name)
}
