// SendProp type, flags, and value decoder.
//
// Each SendProp in a SendTable has a type, flags, and type-specific
// parameters (bit count, low/high values, element count). At decode time we
// read a value from the bit stream according to those parameters.
//
// Reference: Source SDK `dt_common.h` and `dt_recv_decoder.cpp`.

use super::bitreader::BitReader;

// SendPropType - matches Source's SP_TYPE_* enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendPropType {
    Int       = 0,
    Float     = 1,
    Vector    = 2,
    VectorXY  = 3,
    String    = 4,
    Array     = 5,
    DataTable = 6,
    Int64     = 7,
}

impl SendPropType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(SendPropType::Int),
            1 => Some(SendPropType::Float),
            2 => Some(SendPropType::Vector),
            3 => Some(SendPropType::VectorXY),
            4 => Some(SendPropType::String),
            5 => Some(SendPropType::Array),
            6 => Some(SendPropType::DataTable),
            7 => Some(SendPropType::Int64),
            _ => None,
        }
    }
}

// SendProp flag bit values - exactly match Source SDK + tf-demo-parser's enum.
// Earlier guesses about higher bit positions were wrong; flags only span 16 bits.
// Note that NormalVarInt (1<<5) is overloaded: it means "Normal" for floats /
// vectors but "VarInt" for ints. The decoder dispatches based on prop_type.
pub const SPROP_UNSIGNED:                u32 = 1 << 0;
pub const SPROP_COORD:                   u32 = 1 << 1;
pub const SPROP_NOSCALE:                 u32 = 1 << 2;
pub const SPROP_ROUNDDOWN:               u32 = 1 << 3;
pub const SPROP_ROUNDUP:                 u32 = 1 << 4;
pub const SPROP_NORMAL_OR_VARINT:        u32 = 1 << 5;
pub const SPROP_EXCLUDE:                 u32 = 1 << 6;
pub const SPROP_XYZE:                    u32 = 1 << 7;
pub const SPROP_INSIDEARRAY:             u32 = 1 << 8;
pub const SPROP_PROXY_ALWAYS_YES:        u32 = 1 << 9;
pub const SPROP_CHANGES_OFTEN:           u32 = 1 << 10;
pub const SPROP_IS_VECTOR_ELEM:          u32 = 1 << 11;
pub const SPROP_COLLAPSIBLE:             u32 = 1 << 12;
pub const SPROP_COORD_MP:                u32 = 1 << 13;
pub const SPROP_COORD_MP_LOW_PRECISION:  u32 = 1 << 14;
pub const SPROP_COORD_MP_INTEGRAL:       u32 = 1 << 15;
// Cell-coord flags don't exist in TF2's 16-bit flag space - Portal 2 / Alien
// Swarm-era games add them. We map them onto spare high bits at flatten time
// (see datatable.rs normalize_portal2_flags) so the 16-bit TF2 positions stay
// untouched. Used for the cell-relative origin encoding (m_vecOrigin with the
// cell-coord proxy).
pub const SPROP_CELL_COORD:              u32 = 1 << 20;
pub const SPROP_CELL_COORD_LOW_PRECISION:u32 = 1 << 21;
pub const SPROP_CELL_COORD_INTEGRAL:     u32 = 1 << 22;

// Back-compat alias - Normal for vectors uses the same flag as VarInt for ints.
pub const SPROP_NORMAL: u32 = SPROP_NORMAL_OR_VARINT;
pub const SPROP_VARINT: u32 = SPROP_NORMAL_OR_VARINT;

// Coord constants from Source SDK
const COORD_INTEGER_BITS: u32 = 14;
const COORD_FRACTIONAL_BITS: u32 = 5;
const COORD_DENOMINATOR: f32 = (1u32 << COORD_FRACTIONAL_BITS) as f32;
const COORD_RESOLUTION: f32 = 1.0 / COORD_DENOMINATOR;

const COORD_INTEGER_BITS_MP: u32 = 11;
const COORD_FRACTIONAL_BITS_MP_LOWPRECISION: u32 = 3;
const COORD_DENOMINATOR_LOWPRECISION: f32 =
    (1u32 << COORD_FRACTIONAL_BITS_MP_LOWPRECISION) as f32;
const COORD_RESOLUTION_LOWPRECISION: f32 = 1.0 / COORD_DENOMINATOR_LOWPRECISION;

const NORMAL_FRACTIONAL_BITS: u32 = 11;
const NORMAL_DENOMINATOR: f32 = ((1u32 << NORMAL_FRACTIONAL_BITS) - 1) as f32;
const NORMAL_RESOLUTION: f32 = 1.0 / NORMAL_DENOMINATOR;

/// A flattened SendProp definition - the parameters needed to read a value
/// from the bit stream.
#[derive(Debug, Clone)]
pub struct SendPropDef {
    pub name: String,
    pub table_name: String,
    pub prop_type: SendPropType,
    pub flags: u32,
    pub bit_count: u32,
    pub low_value: f32,
    pub high_value: f32,
    pub element_count: u16,
    pub element_def: Option<Box<SendPropDef>>, // for Array props
    pub priority: u8, // proto-4 explicit priority byte (0 on proto-3)
    /// Name of the immediate parent SendProp when this leaf came from a nested
    /// (non-collapsible) DataTable - i.e. the array name for engine-generated
    /// SendPropArray element tables, whose leaves are bare numeric indices
    /// ("000".."063"). `None` for top-level / collapsed props. Lets the
    /// CCSPlayerResource scoreboard arrays (m_iScore[i], m_iDeaths[i], …) be
    /// recovered on the Source 1 path, where flattening otherwise drops the
    /// array name and keeps only the per-slot index.
    pub array_parent: Option<String>,
}

/// Decoded prop value.
#[derive(Debug, Clone)]
pub enum PropValue {
    Int(i64),
    Float(f32),
    Vector(f32, f32, f32),
    VectorXY(f32, f32),
    String(String),
    Array(Vec<PropValue>),
}

impl PropValue {
    pub fn as_i64(&self) -> Option<i64> {
        if let PropValue::Int(v) = self { Some(*v) } else { None }
    }
    pub fn as_f32(&self) -> Option<f32> {
        if let PropValue::Float(v) = self { Some(*v) } else { None }
    }
    pub fn as_vector(&self) -> Option<(f32, f32, f32)> {
        if let PropValue::Vector(x, y, z) = self { Some((*x, *y, *z)) } else { None }
    }
    pub fn as_vector_xy(&self) -> Option<(f32, f32)> {
        if let PropValue::VectorXY(x, y) = self { Some((*x, *y)) } else { None }
    }
}

/// Decode a single prop value from the bit stream.
pub fn decode_prop(prop: &SendPropDef, br: &mut BitReader) -> Option<PropValue> {
    match prop.prop_type {
        SendPropType::Int     => decode_int(prop, br).map(PropValue::Int),
        SendPropType::Int64   => decode_int64(prop, br).map(PropValue::Int),
        SendPropType::Float   => decode_float(prop, br).map(PropValue::Float),
        SendPropType::Vector  => decode_vector(prop, br),
        SendPropType::VectorXY=> decode_vector_xy(prop, br),
        SendPropType::String  => decode_string(br).map(PropValue::String),
        SendPropType::Array   => decode_array(prop, br),
        SendPropType::DataTable => None, // sub-table props aren't leaf-decoded
    }
}

fn decode_int(prop: &SendPropDef, br: &mut BitReader) -> Option<i64> {
    if prop.flags & SPROP_VARINT != 0 {
        // Source's "VarInt" - protobuf-style, 7 bits per byte
        let unsigned = prop.flags & SPROP_UNSIGNED != 0;
        let mut result: u64 = 0;
        for shift in (0..70).step_by(7) {
            let b = br.read_bits(8)? as u64;
            result |= (b & 0x7F) << shift;
            if b & 0x80 == 0 { break; }
            if shift >= 35 { return None; } // guard against runaway
        }
        if unsigned {
            Some(result as i64)
        } else {
            // ZigZag decode
            Some(((result >> 1) as i64) ^ -((result & 1) as i64))
        }
    } else if prop.flags & SPROP_UNSIGNED != 0 {
        Some(br.read_bits(prop.bit_count)? as i64)
    } else {
        Some(br.read_signed(prop.bit_count)? as i64)
    }
}

fn decode_int64(prop: &SendPropDef, br: &mut BitReader) -> Option<i64> {
    if prop.flags & SPROP_VARINT != 0 {
        decode_int(prop, br)
    } else {
        let neg = if prop.flags & SPROP_UNSIGNED == 0 { br.read_bits(1)? != 0 } else { false };
        let lo = br.read_bits(32)? as u64;
        let hi_bits = prop.bit_count.saturating_sub(32 + if prop.flags & SPROP_UNSIGNED == 0 { 1 } else { 0 });
        let hi = br.read_bits(hi_bits)? as u64;
        let val = (hi << 32) | lo;
        Some(if neg { -(val as i64) } else { val as i64 })
    }
}

fn decode_float(prop: &SendPropDef, br: &mut BitReader) -> Option<f32> {
    // Flag precedence matches tf-demo-parser's FloatDefinition::new: Coord
    // beats CoordMP beats CoordMPLowPrecision beats CoordMPIntegral beats
    // NoScale beats Normal beats Scaled (default).
    if prop.flags & SPROP_COORD != 0 { return decode_coord(br); }
    if prop.flags & SPROP_COORD_MP != 0 { return decode_coord_mp(br, false, false); }
    if prop.flags & SPROP_COORD_MP_LOW_PRECISION != 0 { return decode_coord_mp(br, false, true); }
    if prop.flags & SPROP_COORD_MP_INTEGRAL != 0 { return decode_coord_mp(br, true, false); }
    // Cell-coord (Portal 2 / Alien Swarm era): in-cell offset. bit_count is
    // the integer-part width; fractional part is 5 bits (3 for low-precision).
    if prop.flags & SPROP_CELL_COORD != 0 { return decode_cell_coord(br, prop.bit_count, false, false); }
    if prop.flags & SPROP_CELL_COORD_LOW_PRECISION != 0 { return decode_cell_coord(br, prop.bit_count, false, true); }
    if prop.flags & SPROP_CELL_COORD_INTEGRAL != 0 { return decode_cell_coord(br, prop.bit_count, true, false); }
    // (param order matches tf-demo-parser: is_integral, low_precision)
    if prop.flags & SPROP_NOSCALE != 0 { return br.read_bit_float(); }
    if prop.flags & SPROP_NORMAL != 0 { return decode_normal(br); }
    // Default: bit-packed quantised float in [low, high]
    let raw = br.read_bits(prop.bit_count)?;
    let range = prop.high_value - prop.low_value;
    let denominator = ((1u32 << prop.bit_count) - 1) as f32;
    Some(prop.low_value + (raw as f32) * (range / denominator))
}

fn decode_coord(br: &mut BitReader) -> Option<f32> {
    let has_int = br.read_bool()?;
    let has_frac = br.read_bool()?;
    if !has_int && !has_frac { return Some(0.0); }
    let sign = br.read_bool()?;
    let int_part = if has_int { (br.read_bits(COORD_INTEGER_BITS)? + 1) as f32 } else { 0.0 };
    let frac_part = if has_frac { br.read_bits(COORD_FRACTIONAL_BITS)? as f32 * COORD_RESOLUTION } else { 0.0 };
    let mag = int_part + frac_part;
    Some(if sign { -mag } else { mag })
}

// Reads a CoordMP value. Param order matches tf-demo-parser's
// read_bit_coord_mp(stream, is_integral, low_precision) exactly.
fn decode_coord_mp(br: &mut BitReader, is_integral: bool, low_precision: bool) -> Option<f32> {
    let mut value: f32 = 0.0;
    let mut is_negative = false;

    let in_bounds = br.read_bool()?;
    let has_int_val = br.read_bool()?;

    if is_integral {
        if has_int_val {
            is_negative = br.read_bool()?;
            let bits = if in_bounds { COORD_INTEGER_BITS_MP } else { COORD_INTEGER_BITS };
            value = (br.read_bits(bits)? + 1) as f32;
        }
    } else {
        is_negative = br.read_bool()?;
        if has_int_val {
            let bits = if in_bounds { COORD_INTEGER_BITS_MP } else { COORD_INTEGER_BITS };
            value = (br.read_bits(bits)? + 1) as f32;
        }
        let frac_bits = if low_precision { 3 } else { 5 };
        let frac_val = br.read_bits(frac_bits)?;
        value += (frac_val as f32) / ((1u32 << frac_bits) as f32);
    }

    if is_negative { value = -value; }
    Some(value)
}

// Cell-coord: an unsigned in-cell offset. Unlike the signed Coord/CoordMP
// readers there's no sign bit - cell coordinates are always non-negative.
// Reference: Source SDK `bitbuf.cpp` ReadBitCellCoord.
fn decode_cell_coord(br: &mut BitReader, bits: u32, is_integral: bool, low_precision: bool) -> Option<f32> {
    if is_integral {
        let v = br.read_bits(bits)?;
        Some(v as f32)
    } else {
        let int_val = br.read_bits(bits)?;
        let frac_bits = if low_precision { 3 } else { 5 };
        let frac_val = br.read_bits(frac_bits)?;
        let resolution = 1.0 / ((1u32 << frac_bits) as f32);
        Some(int_val as f32 + frac_val as f32 * resolution)
    }
}

fn decode_normal(br: &mut BitReader) -> Option<f32> {
    let sign = br.read_bool()?;
    let frac = br.read_bits(NORMAL_FRACTIONAL_BITS)? as f32;
    let mag = frac * NORMAL_RESOLUTION;
    Some(if sign { -mag } else { mag })
}

fn decode_vector(prop: &SendPropDef, br: &mut BitReader) -> Option<PropValue> {
    let x = decode_float(prop, br)?;
    let y = decode_float(prop, br)?;
    let z = if prop.flags & SPROP_NORMAL != 0 {
        // Normal: z reconstructed from sign + sqrt(1 - x^2 - y^2)
        let neg_z = br.read_bool()?;
        let sumsq = x * x + y * y;
        let mut z = if sumsq < 1.0 { (1.0 - sumsq).sqrt() } else { 0.0 };
        if neg_z { z = -z; }
        z
    } else {
        decode_float(prop, br)?
    };
    Some(PropValue::Vector(x, y, z))
}

fn decode_vector_xy(prop: &SendPropDef, br: &mut BitReader) -> Option<PropValue> {
    let x = decode_float(prop, br)?;
    let y = decode_float(prop, br)?;
    Some(PropValue::VectorXY(x, y))
}

const DT_MAX_STRING_BUFFERSIZE: u32 = 512;
const DT_MAX_STRING_BITS: u32 = 9; // log2(DT_MAX_STRING_BUFFERSIZE) = 9

fn decode_string(br: &mut BitReader) -> Option<String> {
    let len = br.read_bits(DT_MAX_STRING_BITS)?;
    if len > DT_MAX_STRING_BUFFERSIZE { return None; }
    let mut bytes = Vec::with_capacity(len as usize);
    for _ in 0..len {
        bytes.push(br.read_byte()?);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn decode_array(prop: &SendPropDef, br: &mut BitReader) -> Option<PropValue> {
    let num_bits = bits_for(prop.element_count as u32);
    let count = br.read_bits(num_bits)?;
    let element_def = prop.element_def.as_ref()?;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        out.push(decode_prop(element_def, br)?);
    }
    Some(PropValue::Array(out))
}

fn bits_for(n: u32) -> u32 {
    let mut bits = 1;
    while (1u32 << bits) - 1 < n { bits += 1; }
    bits
}
