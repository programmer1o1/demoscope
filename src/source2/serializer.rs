// FlattenedSerializer model + field decoders.
//
// Ported from dotabuff/manta `field.go`, `field_type.go`, `serializer.go`,
// `sendtable.go`, `field_decoder.go`. Parses `CSVCMsg_FlattenedSerializer` into
// an arena of fields/serializers, assigns each field a "model" (simple / array /
// table, fixed / variable) and a value decoder, and resolves a field-path to the
// decoder that reads the next value off the bit stream.

use std::collections::HashMap;

use super::bitreader::BitReader;
use super::fieldpath::FieldPath;
use super::quantizedfloat::QuantizedFloatDecoder;
use super::super::protobuf::{Reader, Value};

/// A decoded networked value. Only the variants used for tracks carry meaning;
/// the rest exist so the stream stays in sync as every field is read.
#[derive(Clone, Debug)]
pub enum FieldValue {
    Bool(bool),
    I32(i32),
    U64(u64),
    F32(f32),
    Vec3([f32; 3]),
    VecN(Vec<f32>),
    Str(String),
}

impl FieldValue {
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            FieldValue::F32(v) => Some(*v),
            FieldValue::I32(v) => Some(*v as f32),
            FieldValue::U64(v) => Some(*v as f32),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            FieldValue::U64(v) => Some(*v),
            FieldValue::I32(v) => Some(*v as u64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            FieldValue::Bool(b) => Some(*b),
            FieldValue::I32(v) => Some(*v != 0),
            FieldValue::U64(v) => Some(*v != 0),
            _ => None,
        }
    }
    pub fn as_vec3(&self) -> Option<[f32; 3]> {
        match self {
            FieldValue::Vec3(v) => Some(*v),
            FieldValue::VecN(v) if v.len() >= 3 => Some([v[0], v[1], v[2]]),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FieldValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub enum Decoder {
    Bool,
    String,
    BinaryBlock,
    Signed,
    Unsigned,
    Unsigned64,
    Fixed64,
    Noscale,
    Coord,
    SimTime,
    Ammo,
    Component,
    GameModeRules,
    Quantized(QuantizedFloatDecoder),
    Vector(usize, Box<Decoder>),
    VectorNormal,
    QanglePitchYaw(u32), // encoder "qangle_pitch_yaw": pitch + yaw only (2 × n bits), roll = 0
    Qangle3(u32),   // 3 × n-bit quantized angle (or noscale when n == 32)
    QangleVar,      // bool x/y/z then read_coord each
    QanglePres,     // bool x/y/z then 20-bit coord each
}

/// One component of a fixed-bit-count QAngle. 32 bits is a raw noscale float;
/// otherwise it's a quantized angle: `read_bits(n) * 360 / 2^n` (manta readAngle).
fn read_qangle_comp(r: &mut BitReader, bits: u32) -> f32 {
    if bits == 32 {
        f32::from_bits(r.read_bits(32))
    } else {
        r.read_bits(bits) as f32 * 360.0 / (1u32 << bits) as f32
    }
}

fn read_coord_pres(r: &mut BitReader) -> f32 {
    r.read_bits(20) as f32 * 360.0 / (1u32 << 20) as f32 - 180.0
}

pub fn decode_value(d: &Decoder, r: &mut BitReader) -> FieldValue {
    match d {
        Decoder::Bool => FieldValue::Bool(r.read_bit()),
        Decoder::String => FieldValue::Str(r.read_string()),
        Decoder::BinaryBlock => {
            let n = r.read_var_u32() as usize;
            let b = r.read_bytes(n);
            FieldValue::Str(String::from_utf8_lossy(&b).into_owned())
        }
        Decoder::Signed => FieldValue::I32(r.read_var_i32()),
        Decoder::Unsigned => FieldValue::U64(r.read_var_u32() as u64),
        Decoder::Unsigned64 => FieldValue::U64(r.read_var_u64()),
        Decoder::Fixed64 => FieldValue::U64(r.read_le_u64()),
        Decoder::Noscale => FieldValue::F32(r.read_float_noscale()),
        Decoder::Coord => FieldValue::F32(r.read_coord()),
        Decoder::SimTime => FieldValue::F32(r.read_var_u32() as f32 * (1.0 / 30.0)),
        Decoder::Ammo => {
            let a = r.read_var_u32();
            FieldValue::U64(if a > 0 { (a - 1) as u64 } else { 0 })
        }
        Decoder::Component => FieldValue::U64(r.read_bits(1) as u64),
        Decoder::GameModeRules => {
            let b = r.read_bit();
            let _ = r.read_ubit_var();
            FieldValue::Bool(b)
        }
        Decoder::Quantized(q) => FieldValue::F32(q.decode(r)),
        Decoder::Vector(n, inner) => {
            let mut v = Vec::with_capacity(*n);
            for _ in 0..*n {
                v.push(decode_value(inner, r).as_f32().unwrap_or(0.0));
            }
            FieldValue::VecN(v)
        }
        Decoder::VectorNormal => FieldValue::Vec3(r.read_3bit_normal()),
        Decoder::QanglePitchYaw(bits) => {
            // Only pitch and yaw are transmitted; roll is always 0.
            FieldValue::Vec3([read_qangle_comp(r, *bits), read_qangle_comp(r, *bits), 0.0])
        }
        Decoder::Qangle3(bits) => FieldValue::Vec3([
            read_qangle_comp(r, *bits),
            read_qangle_comp(r, *bits),
            read_qangle_comp(r, *bits),
        ]),
        Decoder::QangleVar => {
            let (rx, ry, rz) = (r.read_bit(), r.read_bit(), r.read_bit());
            FieldValue::Vec3([
                if rx { r.read_coord() } else { 0.0 },
                if ry { r.read_coord() } else { 0.0 },
                if rz { r.read_coord() } else { 0.0 },
            ])
        }
        Decoder::QanglePres => {
            let (rx, ry, rz) = (r.read_bit(), r.read_bit(), r.read_bit());
            FieldValue::Vec3([
                if rx { read_coord_pres(r) } else { 0.0 },
                if ry { read_coord_pres(r) } else { 0.0 },
                if rz { read_coord_pres(r) } else { 0.0 },
            ])
        }
    }
}

#[derive(Clone)]
pub struct FieldType {
    pub base_type: String,
    pub generic: Option<Box<FieldType>>,
    pub pointer: bool,
    pub count: i32,
}

fn parse_field_type(name: &str) -> FieldType {
    // base is everything up to the first of '<', '[', '*'.
    let base_end = name.find(['<', '[', '*']).unwrap_or(name.len());
    let base_type = name[..base_end].trim().to_string();

    let generic = match (name.find('<'), name.rfind('>')) {
        (Some(a), Some(b)) if b > a => Some(Box::new(parse_field_type(name[a + 1..b].trim()))),
        _ => None,
    };

    let pointer = name.contains('*');

    let mut count = 0;
    if let (Some(a), Some(b)) = (name.find('['), name.rfind(']')) {
        if b > a {
            let inner = name[a + 1..b].trim();
            count = match inner {
                "MAX_ITEM_STOCKS" => 8,
                "MAX_ABILITY_DRAFT_ABILITIES" => 48,
                _ => inner.parse::<i32>().ok().filter(|n| *n > 0).unwrap_or(if inner.is_empty() { 0 } else { 1024 }),
            };
        }
    }

    FieldType { base_type, generic, pointer, count }
}

/// Resolved field shape (mirrors demoparser's Field enum). Array/Vector wrap an
/// element field and resolve `get_inner(idx)` to that element regardless of idx.
#[derive(Clone)]
pub enum Kind {
    Value(Decoder),
    Pointer(usize),    // serializer index
    Serializer(usize), // serializer index
    Array(Box<Kind>),
    Vector(Box<Kind>),
    None,
}

pub struct Field {
    var_name: String,
    var_type: String,
    encoder: String,
    encode_flags: Option<i32>,
    bit_count: Option<i32>,
    low: Option<f32>,
    high: Option<f32>,
    serializer_name: String,
    serializer_version: i32,
    field_type: FieldType,
}

pub struct Serializer {
    pub name: String,
    pub version: i32,
    pub fields: Vec<usize>, // indices into Tables.fields (metadata)
    pub kinds: Vec<Kind>,   // resolved per-serializer (version-correct links)
}

pub struct Class {
    pub class_id: i32,
    pub name: String,
    pub serializer_idx: Option<usize>,
}

pub struct Tables {
    pub fields: Vec<Field>,
    pub serializers: Vec<Serializer>,
    pub by_name: HashMap<String, usize>,
    pub classes_by_id: HashMap<i32, Class>,
    pub class_id_size: u32,
}

/// baseTypes that are pointer-like even without a `*` in the type string
/// (demoparser `is_pointer`).
fn is_pointer_type(base: &str) -> bool {
    matches!(
        base,
        "CBodyComponent" | "CLightComponent" | "CPhysicsComponent" | "CRenderComponent"
            | "CPlayerLocalData"
    )
}

fn float_decoder(f: &Field) -> Decoder {
    // var_name special cases (demoparser find_float_decoder).
    match f.var_name.as_str() {
        "m_flSimulationTime" | "m_flAnimTime" => return Decoder::SimTime,
        _ => {}
    }
    match f.encoder.as_str() {
        "coord" => return Decoder::Coord,
        "m_flSimulationTime" => return Decoder::SimTime,
        _ => {}
    }
    match f.bit_count {
        Some(b) if b > 0 && b < 32 => {
            Decoder::Quantized(QuantizedFloatDecoder::new(f.bit_count, f.encode_flags, f.low, f.high))
        }
        _ => Decoder::Noscale,
    }
}

fn vector_decoder(f: &Field, n: usize) -> Decoder {
    if n == 3 && f.encoder == "normal" {
        return Decoder::VectorNormal;
    }
    match float_decoder(f) {
        Decoder::Noscale => Decoder::Vector(n, Box::new(Decoder::Noscale)),
        Decoder::Coord => Decoder::Vector(n, Box::new(Decoder::Coord)),
        _ => Decoder::VectorNormal, // demoparser fallback ("should not happen")
    }
}

fn uint_decoder(f: &Field) -> Decoder {
    if f.encoder == "fixed64" { Decoder::Fixed64 } else { Decoder::Unsigned64 }
}

fn qangle_decoder(f: &Field) -> Decoder {
    // Drive purely off bit_count/encoder, matching manta/demoparser. (The old
    // by-name `m_angEyeAngles => QanglePitchYaw` case forced a 96-bit read; it
    // never fired for CS2 — whose eye angles use `qangle_precise`, overridden
    // below — but desynced Deadlock, where they use `qangle` with bit_count=11.)
    let bits = f.bit_count.unwrap_or(0);
    if f.encoder == "qangle_pitch_yaw" {
        // pitch + yaw only (roll = 0); 2 × bit_count bits, not 3.
        return Decoder::QanglePitchYaw(bits.max(0) as u32);
    }
    match f.bit_count {
        Some(b) if b != 0 => Decoder::Qangle3(b as u32),
        _ => Decoder::QangleVar,
    }
}

fn find_decoder(f: &Field) -> Decoder {
    // Mirrors demoparser: var_name override, then base-type map, then by
    // float/vector/qangle/handle, then post overrides.
    if f.var_name == "m_iClip1" {
        return Decoder::Ammo;
    }
    let mut dec = match base_type_decoder(&f.field_type.base_type) {
        Some(d) => d,
        None => {
            let name = f
                .field_type
                .generic
                .as_ref()
                .map(|g| g.base_type.as_str())
                .unwrap_or(f.field_type.base_type.as_str());
            match name {
                "float32" | "CNetworkedQuantizedFloat" => float_decoder(f),
                "Vector" | "VectorWS" => vector_decoder(f, 3),
                "Vector2D" => vector_decoder(f, 2),
                "Vector4D" => vector_decoder(f, 4),
                "uint64" => uint_decoder(f),
                "QAngle" => qangle_decoder(f),
                "CHandle" | "CEntityHandle" => Decoder::Unsigned,
                "CStrongHandle" => uint_decoder(f),
                // Unknown type → 64-bit varint, NOT 32-bit. read_var_u32 stops
                // after 5 bytes, so any unknown that is really a 64-bit value
                // (e.g. Deadlock's ResourceId_t, a 64-bit resource hash) would
                // leave continuation bytes unread and desync the whole packet.
                // read_var_u64 consumes any-length varint, and small enum values
                // encode identically, so this is strictly safer as the default.
                other => base_type_decoder(other).unwrap_or(Decoder::Unsigned64),
            }
        }
    };
    // Post-find overrides (demoparser generate_field_data).
    match f.var_name.as_str() {
        "m_PredFloatVariables" | "m_OwnerOnlyPredNetFloatVariables" => dec = Decoder::Noscale,
        "m_OwnerOnlyPredNetVectorVariables" | "m_PredVectorVariables" => {
            dec = Decoder::Vector(3, Box::new(Decoder::Noscale))
        }
        "m_pGameModeRules" => dec = Decoder::GameModeRules,
        _ => {}
    }
    if f.encoder == "qangle_precise" {
        dec = Decoder::QanglePres;
    }
    dec
}

/// demoparser BASETYPE_DECODERS. Many CS2 enum types decode as 64-bit varints.
fn base_type_decoder(base: &str) -> Option<Decoder> {
    Some(match base {
        "bool" => Decoder::Bool,
        "char" | "CUtlString" | "CUtlSymbolLarge" | "CGlobalSymbol" => Decoder::String,
        "CUtlBinaryBlock" => Decoder::BinaryBlock,
        "int8" | "int16" | "int32" | "int64" => Decoder::Signed,
        "uint8" | "uint16" | "uint32" | "color32" | "Color" | "CGameSceneNodeHandle"
        | "CUtlStringToken" => Decoder::Unsigned,
        "GameTime_t" | "Quaternion" | "CTransform" => Decoder::Noscale,
        "CBodyComponent" | "CPhysicsComponent" | "CRenderComponent" => Decoder::Component,
        "HSequence" | "AttachmentHandle_t" | "CEntityIndex" | "MoveCollide_t" | "MoveType_t"
        | "RenderMode_t" | "RenderFx_t" | "SolidType_t" | "SurroundingBoundsType_t"
        | "ModelConfigHandle_t" | "NPC_STATE" | "StanceType_t" | "AbilityPathType_t"
        | "WeaponState_t" | "DoorState_t" | "RagdollBlendDirection" | "BeamType_t"
        | "BeamClipStyle_t" | "EntityDisolveType_t" | "tablet_skin_state_t" | "CStrongHandle"
        | "CSWeaponMode" | "ESurvivalSpawnTileState" | "SpawnStage_t"
        | "ESurvivalGameRuleDecision_t" | "RelativeDamagedDirection_t" | "CSPlayerState"
        | "MedalRank_t" | "CSPlayerBlockingUseAction_t" | "MoveMountingAmount_t"
        | "QuestProgress::Reason" => Decoder::Unsigned64,
        _ => return None,
    })
}

/// Build a field's resolved `Kind` (demoparser create_field + find_category).
/// `ser_idx` is the serializer this field links to (version-correct), if any.
fn build_kind(f: &Field, ser_idx: Option<usize>) -> Kind {
    let ft = &f.field_type;
    let is_pointer = ft.pointer || is_pointer_type(&ft.base_type);
    let is_vector = ser_idx.is_some()
        || ft.base_type == "CUtlVector"
        || ft.base_type == "CNetworkUtlVectorBase";
    let is_array = ft.count > 0 && ft.base_type != "char";

    let element = if let Some(sidx) = ser_idx {
        if is_pointer { Kind::Pointer(sidx) } else { Kind::Serializer(sidx) }
    } else {
        Kind::Value(find_decoder(f))
    };

    if is_pointer {
        element
    } else if is_vector {
        Kind::Vector(Box::new(element))
    } else if is_array {
        Kind::Array(Box::new(element))
    } else {
        element
    }
}

impl Tables {
    /// Resolve a field path to the decoder that reads its value.
    pub fn decoder_for_path<'a>(&'a self, ser_idx: usize, fp: &FieldPath, _pos: usize) -> Option<&'a Decoder> {
        // Base decoders for paths that terminate on a container/struct rather
        // than a leaf, matching demoparser's get_decoder_from_field:
        //   Pointer -> boolean (1 bit; or GameModeRules for CCSGameModeRules)
        //   Vector  -> unsigned (the element count)
        static UNSIGNED: Decoder = Decoder::Unsigned;
        static BOOL: Decoder = Decoder::Bool;
        static GAME_MODE: Decoder = Decoder::GameModeRules;
        let ser = self.serializers.get(ser_idx)?;
        let mut kind = ser.kinds.get(*fp.path.first()? as usize)?;
        for i in 1..=fp.last {
            kind = self.get_inner(kind, fp.path[i] as usize)?;
        }
        match kind {
            Kind::Value(d) => Some(d),
            Kind::Pointer(s) => {
                if self.serializers.get(*s).map(|x| x.name.as_str()) == Some("CCSGameModeRules") {
                    Some(&GAME_MODE)
                } else {
                    Some(&BOOL)
                }
            }
            Kind::Vector(_) => Some(&UNSIGNED),
            Kind::Serializer(_) => Some(&UNSIGNED), // shouldn't terminate here
            _ => None,
        }
    }

    /// The raw `high_value` of the leaf field a dotted name resolves to (e.g.
    /// "CBodyComponent.m_vecX"). Used to derive the cell width for world coords:
    /// the quantized cell offset spans [0, 2 × cell_width), so cell_width = high/2.
    pub fn field_high(&self, ser_idx: usize, name: &str) -> Option<f32> {
        let fp = self.path_for_name(ser_idx, name)?;
        let mut cur = ser_idx;
        let mut leaf: Option<usize> = None;
        for i in 0..=fp.last {
            let ser = self.serializers.get(cur)?;
            let fi = *ser.fields.get(fp.path[i] as usize)?;
            leaf = Some(fi);
            if i < fp.last {
                cur = inner_serializer(&ser.kinds[fp.path[i] as usize])?;
            }
        }
        self.fields.get(leaf?)?.high
    }

    fn get_inner<'a>(&'a self, kind: &'a Kind, idx: usize) -> Option<&'a Kind> {
        match kind {
            Kind::Array(e) | Kind::Vector(e) => Some(e),
            Kind::Pointer(s) | Kind::Serializer(s) => self.serializers.get(*s)?.kinds.get(idx),
            _ => None,
        }
    }

    /// Build a field path for a dotted name like "CBodyComponent.m_cellX".
    pub fn path_for_name(&self, ser_idx: usize, name: &str) -> Option<FieldPath> {
        let mut fp = FieldPath { path: [0; 7], last: 0, done: false, overflow: false };
        if self.ser_path_for_name(ser_idx, &mut fp, name) {
            Some(fp)
        } else {
            None
        }
    }

    fn ser_path_for_name(&self, ser_idx: usize, fp: &mut FieldPath, name: &str) -> bool {
        let ser = match self.serializers.get(ser_idx) {
            Some(s) => s,
            None => return false,
        };
        for (i, &fi) in ser.fields.iter().enumerate() {
            let vn = &self.fields[fi].var_name;
            if name == vn {
                fp.path[fp.last] = i as i32;
                return true;
            }
            let prefix = format!("{}.", vn);
            if let Some(rest) = name.strip_prefix(&prefix) {
                fp.path[fp.last] = i as i32;
                fp.last += 1;
                if let Some(s) = inner_serializer(&ser.kinds[i]) {
                    return self.ser_path_for_name(s, fp, rest);
                }
                return false;
            }
        }
        false
    }
}

fn inner_serializer(kind: &Kind) -> Option<usize> {
    match kind {
        Kind::Pointer(s) | Kind::Serializer(s) => Some(*s),
        Kind::Array(e) | Kind::Vector(e) => inner_serializer(e),
        _ => None,
    }
}

/// Parse a `CSVCMsg_FlattenedSerializer` body into a `Tables` (classes filled
/// in later from CDemoClassInfo).
pub fn parse_flattened(body: &[u8]) -> Option<Tables> {
    let mut symbols: Vec<String> = Vec::new();
    let mut raw_serializers: Vec<(i32, i32, Vec<usize>)> = Vec::new(); // name_sym, version, field idxs
    let mut raw_fields: Vec<RawField> = Vec::new();

    let mut r = Reader::new(body);
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            1 => {
                if let Some(b) = f.value.as_bytes() {
                    if let Some(s) = parse_serializer(b) {
                        raw_serializers.push(s);
                    }
                }
            }
            2 => {
                if let Some(s) = f.value.as_str() {
                    symbols.push(s.into_owned());
                }
            }
            3 => {
                if let Some(b) = f.value.as_bytes() {
                    raw_fields.push(parse_field(b));
                }
            }
            _ => {}
        }
    }

    let sym = |i: i32| -> String { symbols.get(i as usize).cloned().unwrap_or_default() };
    let sym_opt = |o: Option<i32>| -> String { o.map(|i| sym(i)).unwrap_or_default() };

    // Build the field arena.
    let mut fields: Vec<Field> = Vec::with_capacity(raw_fields.len());
    for rf in &raw_fields {
        let var_type = sym_opt(rf.var_type_sym);
        let field_type = parse_field_type(&var_type);
        fields.push(Field {
            var_name: sym_opt(rf.var_name_sym),
            var_type,
            encoder: sym_opt(rf.var_encoder_sym),
            encode_flags: rf.encode_flags,
            bit_count: rf.bit_count,
            low: rf.low_value,
            high: rf.high_value,
            serializer_name: sym_opt(rf.field_serializer_name_sym),
            serializer_version: rf.field_serializer_version.unwrap_or(0),
            field_type,
        });
    }

    // Pass 1: create all serializers; index by (name, version) — unique — and by
    // name (last wins) for class lookups.
    let mut serializers: Vec<Serializer> = Vec::with_capacity(raw_serializers.len());
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let mut by_id: HashMap<(String, i32), usize> = HashMap::new();
    for (name_sym, version, field_idxs) in &raw_serializers {
        let name = sym(*name_sym);
        let idx = serializers.len();
        by_id.insert((name.clone(), *version), idx);
        by_name.insert(name.clone(), idx);
        serializers.push(Serializer { name, version: *version, fields: field_idxs.clone(), kinds: Vec::new() });
    }

    // Pass 2: resolve each field's link by (serializer_name, field version) — the
    // exact version the field references (versions differ in field count, so a
    // name-only link picks the wrong one and desyncs deep paths).
    for si in 0..serializers.len() {
        let field_idxs = serializers[si].fields.clone();
        let mut kinds = Vec::with_capacity(field_idxs.len());
        for &fi in &field_idxs {
            let f = &fields[fi];
            let ser_idx = if f.serializer_name.is_empty() {
                None
            } else {
                by_id
                    .get(&(f.serializer_name.clone(), f.serializer_version))
                    .copied()
                    .or_else(|| by_name.get(&f.serializer_name).copied())
            };
            kinds.push(build_kind(f, ser_idx));
        }
        serializers[si].kinds = kinds;
    }

    Some(Tables { fields, serializers, by_name, classes_by_id: HashMap::new(), class_id_size: 0 })
}

struct RawField {
    var_type_sym: Option<i32>,
    var_name_sym: Option<i32>,
    bit_count: Option<i32>,
    low_value: Option<f32>,
    high_value: Option<f32>,
    encode_flags: Option<i32>,
    field_serializer_name_sym: Option<i32>,
    field_serializer_version: Option<i32>,
    var_encoder_sym: Option<i32>,
}

fn parse_field(body: &[u8]) -> RawField {
    let mut rf = RawField {
        var_type_sym: None,
        var_name_sym: None,
        bit_count: None,
        low_value: None,
        high_value: None,
        encode_flags: None,
        field_serializer_name_sym: None,
        field_serializer_version: None,
        var_encoder_sym: None,
    };
    let mut r = Reader::new(body);
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            1 => rf.var_type_sym = f.value.as_i32(),
            2 => rf.var_name_sym = f.value.as_i32(),
            3 => rf.bit_count = f.value.as_i32(),
            4 => rf.low_value = f.value.as_f32(),
            5 => rf.high_value = f.value.as_f32(),
            6 => rf.encode_flags = f.value.as_i32(),
            7 => rf.field_serializer_name_sym = f.value.as_i32(),
            8 => rf.field_serializer_version = f.value.as_i32(),
            10 => rf.var_encoder_sym = f.value.as_i32(),
            _ => {}
        }
    }
    rf
}

fn parse_serializer(body: &[u8]) -> Option<(i32, i32, Vec<usize>)> {
    let mut name_sym = 0i32;
    let mut version = 0i32;
    let mut fields_index: Vec<usize> = Vec::new();
    let mut r = Reader::new(body);
    while let Ok(Some(f)) = r.next_field() {
        match f.number {
            1 => name_sym = f.value.as_i32()?,
            2 => version = f.value.as_i32()?,
            3 => match f.value {
                // fields_index is `repeated int32` — packed (one Len blob of
                // varints) or, rarely, one varint per field.
                Value::Len(b) => {
                    let mut rr = Reader::new(b);
                    while !rr.is_empty() {
                        match rr.read_varint() {
                            Ok(v) => fields_index.push(v as usize),
                            Err(_) => break,
                        }
                    }
                }
                _ => {
                    if let Some(v) = f.value.as_u64() {
                        fields_index.push(v as usize);
                    }
                }
            },
            _ => {}
        }
    }
    Some((name_sym, version, fields_index))
}
