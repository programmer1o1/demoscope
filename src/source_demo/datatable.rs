// DEM_DATATABLES parser.
//
// The DEM_DATATABLES packet contains every SendTable definition the demo
// uses, followed by the server-class list. We parse those tables, then
// flatten each server class's prop hierarchy into the ordered leaf-prop
// list that PacketEntities deltas reference by index.
//
// Reference: Source SDK `dt_send.cpp` (build flattened table), `cl_main.cpp`
// (read tables off the wire). Also tf-demo-parser's `flatten.rs`.

use std::collections::{HashMap, HashSet};

use super::bitreader::BitReader;
use super::sendprop::{
    SendPropDef, SendPropType,
    SPROP_CHANGES_OFTEN, SPROP_COLLAPSIBLE, SPROP_EXCLUDE, SPROP_INSIDEARRAY,
    SPROP_COORD_MP, SPROP_COORD_MP_LOW_PRECISION, SPROP_COORD_MP_INTEGRAL,
    SPROP_CELL_COORD, SPROP_CELL_COORD_LOW_PRECISION, SPROP_CELL_COORD_INTEGRAL,
    SPROP_IS_VECTOR_ELEM,
};

// Portal 2 / Alien Swarm SDK use 19-bit networked SendProp flags whose bit
// positions diverge from TF2's 16-bit layout starting at bit 10. Remap them
// to our canonical TF2 positions so flatten() + the prop decoder don't need
// to know which engine produced the table. Reference: Alien Swarm SDK
// `dt_common.h` (SPROP_* defines) + NeKzor/sdp DataTables.ts SendPropFlags.
//
//   AS bit  meaning                       → canonical TF2 bit
//   0-9     UNSIGNED..PROXY_ALWAYS_YES       0-9   (identical)
//   10      IS_A_VECTOR_ELEM                 11
//   11      COLLAPSIBLE                      12
//   12      COORD_MP                         13
//   13      COORD_MP_LOWPRECISION            14
//   14      COORD_MP_INTEGRAL                15
//   15      CELL_COORD                       20 (synthetic high bit)
//   16      CELL_COORD_LOWPRECISION          21
//   17      CELL_COORD_INTEGRAL              22
//   18      CHANGES_OFTEN                    10
fn normalize_portal2_flags(raw: u32) -> u32 {
    let mut out = raw & 0x3FF; // bits 0-9 are identical
    let bit = |b: u32| (raw >> b) & 1 != 0;
    if bit(10) { out |= SPROP_IS_VECTOR_ELEM; }
    if bit(11) { out |= SPROP_COLLAPSIBLE; }
    if bit(12) { out |= SPROP_COORD_MP; }
    if bit(13) { out |= SPROP_COORD_MP_LOW_PRECISION; }
    if bit(14) { out |= SPROP_COORD_MP_INTEGRAL; }
    if bit(15) { out |= SPROP_CELL_COORD; }
    if bit(16) { out |= SPROP_CELL_COORD_LOW_PRECISION; }
    if bit(17) { out |= SPROP_CELL_COORD_INTEGRAL; }
    if bit(18) { out |= SPROP_CHANGES_OFTEN; }
    out
}

/// A raw SendTable as read from the demo (before flattening).
#[derive(Debug, Clone)]
pub struct RawSendTable {
    pub needs_decoder: bool,
    pub name: String,
    pub props: Vec<RawSendPropDef>,
}

/// A SendProp definition as encoded in the demo's data tables.
#[derive(Debug, Clone)]
pub struct RawSendPropDef {
    pub prop_type: SendPropType,
    pub name: String,
    pub flags: u32,
    pub priority: u8,
    pub exclude_dt_name: Option<String>,
    pub data_table_name: Option<String>,
    pub low_value: f32,
    pub high_value: f32,
    pub bit_count: u32,
    pub element_count: u16,
    /// Bound at table-parse time: when this is an Array prop, the previously
    /// seen `InsideArray` element definition is captured here so the decoder
    /// can read N copies of it.
    pub array_element: Option<Box<RawSendPropDef>>,
}

/// Server class entry (one per game-entity class).
#[derive(Debug, Clone)]
pub struct ServerClass {
    pub id: u16,
    pub name: String,
    pub data_table: String,
}

/// Result of parsing a DEM_DATATABLES payload + flattening.
#[derive(Debug, Default)]
pub struct DataTables {
    pub tables: Vec<RawSendTable>,
    pub server_classes: Vec<ServerClass>,
    /// class_id → flat ordered list of leaf SendPropDefs for entity updates.
    pub flat_props: HashMap<u16, Vec<SendPropDef>>,
}

impl DataTables {
    pub fn find_table(&self, name: &str) -> Option<&RawSendTable> {
        self.tables.iter().find(|t| t.name == name)
    }
}

// SendPropFlag wire width: TF2/CS:S use 16 bits, matches tf-demo-parser.
// Alien Swarm SDK's `dt_common.h` defines `SPROP_NUMFLAGBITS_NETWORKED = 19`
// for proto-4 games - but switching to 19 here misaligns the bit cursor on
// L4D2 / Portal 2 / Stanley Parable parse. The exact serialisation differs
// across the proto-4 family (priority byte placement, optional fields) and
// needs reference-parser corroboration before being wired up; keep 16 for
// the proto-3 path until that's done.
const SPROP_NUM_FLAG_BITS: u32 = 16;
const SPROP_NUM_BITS_NETWORKED: u32 = 7;

/// Per-game wire-format quirks for SendTable decoding.
#[derive(Copy, Clone)]
pub struct DataTableQuirks {
    /// Portal 2-engine games (Source 2013 SP lineage) serialise SendProp flags
    /// as 19 networked bits + an 8-bit priority byte (what NeKzor/sdp reads as
    /// "16-bit flags + 11-bit unk"). They also pin MAX_SPLITSCREEN_CLIENTS = 2
    /// and use the Portal 2 net-message ID remap. The Stanley Parable
    /// (game_dir `thestanleyparable`, net_protocol 1000) shares all three even
    /// though its net protocol differs - it ships `CPortal_Player` and decodes
    /// with this flag set (237 classes, real positions).
    ///
    /// NOTE: this flag is ONLY the SendTable flag-encoding (19 + 8). It is
    /// independent of the *container* quirks (splitscreen count, net-message-ID
    /// remap) handled by `is_portal2_engine()`, and of the `m_nBits` field width
    /// (`bit_count_bits`). The proto-4 family mixes these axes independently:
    ///   - L4D1 (net 1041): 16-bit TF2 flags, no priority, m_nBits = 6
    ///   - L4D2 (net 2100): 19-bit AS flags + priority, m_nBits = 6
    ///   - Portal 2 / Stanley: 19-bit AS flags + priority, m_nBits = 7
    /// All verified by parsing to a sane server-class count (222 / 278 / 235).
    pub portal2_extra_bits: bool,
    /// Width of the SendProp `m_nBits` field (PROPINFOBITS_NUMBITS). The L4D
    /// engine uses 6; everything else we support uses 7. A 1-bit miscount here
    /// desyncs the whole table walk on the first numeric prop.
    pub bit_count_bits: u32,
}

impl DataTableQuirks {
    pub fn for_game(game_dir: &str) -> Self {
        // Games whose SendTables use the post-Orange-Box 19-bit flag + 8-bit
        // priority encoding (Portal 2 / Alien Swarm lineage). L4D2 also uses it;
        // L4D1 does NOT (it keeps the older 16-bit TF2 flag layout).
        const EXTRA_FLAG_BITS: &[&str] = &[
            "portal2", "aperturetag", "TWTM",
            "portal_stories", "portalreloaded", "Portal 2 Speedrun Mod",
            "thestanleyparable",
            "left4dead2",
        ];
        // The L4D engine (both games) writes m_nBits as a 6-bit field.
        const NBITS6: &[&str] = &["left4dead", "left4dead2"];
        let eq = |set: &[&str]| set.iter().any(|g| g.eq_ignore_ascii_case(game_dir));
        DataTableQuirks {
            portal2_extra_bits: eq(EXTRA_FLAG_BITS),
            bit_count_bits: if eq(NBITS6) { 6 } else { 7 },
        }
    }
}

/// Portal 2 / Source-2013-SP engine *container* quirks: the net-message-ID
/// remap and MAX_SPLITSCREEN_CLIENTS = 2. Distinct from `portal2_extra_bits`
/// (the SendProp flag format) - L4D shares that flag format but uses the
/// canonical message map and splitscreen = 4, so it is NOT a portal2 engine.
pub fn is_portal2_engine(game_dir: &str) -> bool {
    const PORTAL2_ENGINE: &[&str] = &[
        "portal2", "aperturetag", "TWTM",
        "portal_stories", "portalreloaded", "Portal 2 Speedrun Mod",
        "thestanleyparable",
    ];
    PORTAL2_ENGINE.iter().any(|g| g.eq_ignore_ascii_case(game_dir))
}

/// Parse the DEM_DATATABLES payload. The payload starts with a sequence of
/// SendTables (each preceded by a 1-bit "next table" flag) followed by a
/// 16-bit server-class count and that many (id, name, data_table) entries.
pub fn parse(payload: &[u8], demo_protocol: i32, quirks: DataTableQuirks) -> Option<DataTables> {
    let mut br = BitReader::new(payload);
    let mut tables: Vec<RawSendTable> = Vec::new();

    // Read tables: each is preceded by a "has another table" bit.
    loop {
        let has_table = br.read_bool()?;
        if !has_table { break; }
        let table = read_send_table(&mut br, demo_protocol, quirks)?;
        tables.push(table);
    }

    // Server classes
    let num_classes = br.read_bits(16)? as usize;
    let mut server_classes = Vec::with_capacity(num_classes);
    for _ in 0..num_classes {
        let id = br.read_bits(16)? as u16;
        let name = br.read_cstring(256)?;
        let dt = br.read_cstring(256)?;
        server_classes.push(ServerClass { id, name, data_table: dt });
    }

    // Flatten props per class
    let mut data = DataTables { tables, server_classes, flat_props: HashMap::new() };
    let class_ids: Vec<(u16, String)> = data.server_classes.iter()
        .map(|c| (c.id, c.data_table.clone()))
        .collect();
    let proto4 = demo_protocol >= 4;
    for (cid, dt_name) in class_ids {
        if let Some(flat) = flatten(&data, &dt_name, proto4) {
            data.flat_props.insert(cid, flat);
        }
    }

    Some(data)
}

fn read_send_table(br: &mut BitReader, demo_protocol: i32, quirks: DataTableQuirks) -> Option<RawSendTable> {
    let needs_decoder = br.read_bool()?;
    let name = br.read_cstring(256)?;
    let num_props = br.read_bits(10)? as usize;
    let mut props: Vec<RawSendPropDef> = Vec::with_capacity(num_props);
    // Source pairs Array props with their element definition: the InsideArray
    // prop precedes the Array prop in the table. We pop the pending element
    // into the array prop when we encounter it.
    let mut array_element: Option<RawSendPropDef> = None;
    for _ in 0..num_props {
        let prop = read_send_prop_def(br, demo_protocol, quirks)?;
        if prop.flags & SPROP_INSIDEARRAY != 0 {
            // Stash this for the next Array prop
            array_element = Some(prop);
        } else if prop.prop_type == SendPropType::Array {
            // Bind the previously seen element
            let elem = array_element.take()?;
            let mut bound = prop;
            bound.array_element = Some(Box::new(elem));
            props.push(bound);
        } else {
            props.push(prop);
        }
    }
    Some(RawSendTable { needs_decoder, name, props })
}

fn read_send_prop_def(br: &mut BitReader, demo_protocol: i32, quirks: DataTableQuirks) -> Option<RawSendPropDef> {
    let type_raw = br.read_bits(5)? as u8;
    let prop_type = SendPropType::from_u8(type_raw)?;
    let name = br.read_cstring(256)?;
    let _ = demo_protocol;
    // Portal 2 / Alien Swarm-era engine: SPROP_NUMFLAGBITS_NETWORKED = 19 plus
    // an 8-bit priority byte = 27 bits total. (NeKzor/sdp reads this as
    // "16-bit flags + 11-bit unk" - same 27-bit total, so its table walk
    // completes, but it never splits the fields correctly because it doesn't
    // decode entities. The real split is 19 + 8.) The 19-bit flag space uses
    // different bit positions than TF2's 16-bit space, so we normalise them
    // back to canonical TF2 positions for the shared flatten + decode paths.
    let (flags, priority) = if quirks.portal2_extra_bits {
        // Alien Swarm / Portal 2 / L4D2 lineage: 19 networked flag bits (whose
        // positions diverge from TF2's 16-bit layout) + an 8-bit priority byte.
        let raw = br.read_bits(19)?;
        let prio = br.read_bits(8)? as u8;
        (normalize_portal2_flags(raw), prio)
    } else {
        // TF2 / CS:S / HL2 / L4D1: 16-bit flags, no priority byte.
        (br.read_bits(SPROP_NUM_FLAG_BITS)?, 0u8)
    };

    let mut exclude_dt_name = None;
    let mut data_table_name = None;
    let mut low_value = 0.0;
    let mut high_value = 0.0;
    let mut bit_count = 0;
    let mut element_count = 0;

    if flags & SPROP_EXCLUDE != 0 || prop_type == SendPropType::DataTable {
        // Exclude prop's "table_name" field doubles as the dt name pointer
        let s = br.read_cstring(256)?;
        if flags & SPROP_EXCLUDE != 0 { exclude_dt_name = Some(s); }
        else { data_table_name = Some(s); }
    } else if prop_type == SendPropType::Array {
        element_count = br.read_bits(10)? as u16;
    } else {
        low_value = br.read_bit_float()?;
        high_value = br.read_bit_float()?;
        bit_count = br.read_bits(quirks.bit_count_bits)?;
    }

    Some(RawSendPropDef {
        prop_type, name, flags, priority,
        exclude_dt_name, data_table_name,
        low_value, high_value, bit_count, element_count,
        array_element: None,
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Flattening - produce the ordered leaf-prop list used by PacketEntities.
//
// 1. Walk the table tree depth-first, collecting all non-DataTable props
//    that aren't excluded and don't have PROXY_ALWAYS_YES.
// 2. Stable-sort by priority (and "changes often" treated as priority 64),
//    so the network order matches the server's encoder.

// Matches tf-demo-parser's `flatten_props` exactly:
//   1. Build the exclude set (props on `(table, name)` pairs that downstream
//      tables marked excluded).
//   2. Walk the table tree: sub-table props are pushed into the *global*
//      flat list BEFORE the current table's local leaf props are appended.
//      `Collapsible` sub-tables merge their props into the parent's
//      local list rather than going through the global list - this preserves
//      Source's flattened prop ordering.
//   3. After flattening, move every prop with SPROP_CHANGES_OFTEN to the
//      front of the list (stable order within that group).
fn flatten(data: &DataTables, root_table: &str, proto4: bool) -> Option<Vec<SendPropDef>> {
    let excludes = build_excludes(data, root_table);
    let mut props: Vec<SendPropDef> = Vec::new();
    push_props_end(data, root_table, &excludes, &mut props, &mut Vec::new());

    if proto4 {
        sort_by_priority(&mut props);
    } else {
        // Proto-3: move "changes often" props to the front (stable shuffle).
        let mut start = 0;
        for i in 0..props.len() {
            if props[i].flags & SPROP_CHANGES_OFTEN != 0 {
                if i != start { props.swap(i, start); }
                start += 1;
            }
        }
    }

    Some(props)
}

// Proto-4 (Portal 2 / Alien Swarm era) flatten ordering: props are arranged by
// ascending priority. This mirrors the engine's `SendTable_SortByPriority`
// (Source SDK dt.cpp) *exactly*, which is subtler than "treat CHANGES_OFTEN as
// priority 64":
//
//   for each priority pass `pr` (ascending, over the distinct priorities plus
//   the implicit 64), claim a prop at the first pass where
//       prop.priority == pr  OR  (prop is CHANGES_OFTEN AND pr == 64)
//
// The key consequence: a CHANGES_OFTEN prop whose own priority is *below* 64
// (e.g. m_vecOrigin at priority 2) is claimed at its own low-priority pass via
// the first clause - NOT pulled forward to 64. Only props whose real priority
// is above 64 get hoisted by the second clause. Forcing every CHANGES_OFTEN
// prop to 64 (the previous behaviour) scrambled the flat order for the Portal 2
// player class - verified against an engine.dll runtime bit-trace of
// CDeltaBitsReader (scripts/p2_engine_trace.md).
fn sort_by_priority(props: &mut [SendPropDef]) {
    const CHANGES_OFTEN_PRIORITY: u8 = 64;
    // Distinct priorities present, always including 64 so CHANGES_OFTEN props
    // with priority > 64 are hoisted to the 64 pass (matches the engine, where
    // 64 is always a pass).
    let mut priorities: Vec<u8> = vec![CHANGES_OFTEN_PRIORITY];
    for p in props.iter() {
        if !priorities.contains(&p.priority) { priorities.push(p.priority); }
    }
    priorities.sort_unstable();

    // Valve's two-clause match predicate.
    let matches = |p: &SendPropDef, pr: u8| -> bool {
        p.priority == pr
            || (p.flags & SPROP_CHANGES_OFTEN != 0 && pr == CHANGES_OFTEN_PRIORITY)
    };

    let mut start = 0usize;
    for &pr in &priorities {
        let mut i = start;
        while i < props.len() {
            if matches(&props[i], pr) {
                if i != start { props.swap(start, i); }
                start += 1;
            }
            i += 1;
        }
    }
}

fn build_excludes(data: &DataTables, root_table: &str) -> HashSet<(String, String)> {
    let mut excludes = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    walk_excludes(data, root_table, &mut stack, &mut excludes);
    excludes
}

fn walk_excludes(
    data: &DataTables,
    table_name: &str,
    stack: &mut Vec<String>,
    excludes: &mut HashSet<(String, String)>,
) {
    if stack.iter().any(|n| n == table_name) { return; }
    stack.push(table_name.to_string());
    let Some(table) = data.find_table(table_name) else { stack.pop(); return; };
    for prop in &table.props {
        if prop.flags & SPROP_EXCLUDE != 0 {
            if let Some(dt) = &prop.exclude_dt_name {
                excludes.insert((dt.clone(), prop.name.clone()));
            }
        } else if prop.prop_type == SendPropType::DataTable {
            if let Some(dt) = &prop.data_table_name {
                walk_excludes(data, dt, stack, excludes);
            }
        }
    }
    stack.pop();
}

fn push_props_end(
    data: &DataTables,
    table_name: &str,
    excludes: &HashSet<(String, String)>,
    props: &mut Vec<SendPropDef>,
    table_stack: &mut Vec<String>,
) {
    let mut local_props: Vec<SendPropDef> = Vec::new();
    push_props_collapse(data, table_name, excludes, &mut local_props, props, table_stack);
    props.extend(local_props);
}

fn push_props_collapse(
    data: &DataTables,
    table_name: &str,
    excludes: &HashSet<(String, String)>,
    local_props: &mut Vec<SendPropDef>,
    props: &mut Vec<SendPropDef>,
    table_stack: &mut Vec<String>,
) {
    table_stack.push(table_name.to_string());
    let Some(table) = data.find_table(table_name) else { table_stack.pop(); return; };

    for prop in &table.props {
        if prop.flags & SPROP_EXCLUDE != 0 { continue; }
        // Exclude lookup uses (owner_table, prop_name) - `owner_table` is the
        // table this prop *lives in*, which is `table_name` here.
        if excludes.contains(&(table_name.to_string(), prop.name.clone())) { continue; }

        if prop.prop_type == SendPropType::DataTable {
            if let Some(dt) = &prop.data_table_name {
                if !table_stack.contains(dt) {
                    if prop.flags & SPROP_COLLAPSIBLE != 0 {
                        push_props_collapse(data, dt, excludes, local_props, props, table_stack);
                    } else {
                        push_props_end(data, dt, excludes, props, table_stack);
                    }
                }
            }
        } else {
            // Inside-array element props are absorbed into the Array prop
            // at table-parse time; they never appear standalone in the flat
            // list.
            if prop.flags & SPROP_INSIDEARRAY != 0 { continue; }
            let element_def = prop.array_element.as_ref().map(|e| Box::new(SendPropDef {
                name: e.name.clone(),
                table_name: table_name.to_string(),
                prop_type: e.prop_type,
                flags: e.flags,
                bit_count: e.bit_count,
                low_value: e.low_value,
                high_value: e.high_value,
                element_count: e.element_count,
                element_def: None,
                priority: e.priority,
            }));
            local_props.push(SendPropDef {
                name: prop.name.clone(),
                table_name: table_name.to_string(),
                prop_type: prop.prop_type,
                flags: prop.flags,
                bit_count: prop.bit_count,
                low_value: prop.low_value,
                high_value: prop.high_value,
                element_count: prop.element_count,
                element_def,
                priority: prop.priority,
            });
        }
    }
    table_stack.pop();
}
