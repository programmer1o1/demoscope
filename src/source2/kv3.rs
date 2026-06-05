// KV3 binary reader — the typed key-values blob that Source 2 resource blocks
// (DATA, PHYS, …) are encoded in. We target the current on-disk version 5
// (magic `\x05 3 V K`), which is what CS2 maps ship.
//
// This file currently implements the *container* half — header parse + buffer
// decompression — which the empirical layout check against `de_nuke.vpk`'s
// PHYS block confirmed byte-exact (header is 120 bytes; the first zstd frame
// begins at 120; buffer2's frame begins at 120 + sizeCompressedBuffer1). The
// value-tree decode (objects/arrays/typed buffers) builds on top of this.
//
// Format reference: ValveResourceFormat (BinaryKV3).

use std::io::Read;

fn le_u16(d: &[u8], o: usize) -> u16 { u16::from_le_bytes([d[o], d[o + 1]]) }
fn le_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// KV3 magic for version 5: byte 0 is the version (5), then "3VK".
const KV3_V5_MAGIC: [u8; 4] = [0x05, b'3', b'V', b'K'];

/// Parsed KV3 v5 header. Field names mirror ValveResourceFormat; only the ones
/// we actually consume are documented.
#[derive(Debug, Default)]
pub struct Kv3Header {
    pub compression_method: u32,
    pub compression_dictionary_id: u16,
    pub compression_frame_size: u16,
    pub count_bytes1: u32,
    pub count_bytes4: u32,
    pub count_bytes8: u32,
    pub count_types: u32,
    pub count_objects: u16,
    pub count_arrays: u16,
    pub size_uncompressed_total: u32,
    pub size_compressed_total: u32,
    pub count_blocks: u32,
    pub size_binary_blobs: u32,
    pub count_bytes2: u32,
    pub size_block_compressed_sizes: u32,
    // v5 dual-buffer fields
    pub size_uncompressed_buffer1: u32,
    pub size_compressed_buffer1: u32,
    pub size_uncompressed_buffer2: u32,
    pub size_compressed_buffer2: u32,
    pub count_bytes1_b2: u32,
    pub count_bytes2_b2: u32,
    pub count_bytes4_b2: u32,
    pub count_bytes8_b2: u32,
    pub unk13: u32,
    pub count_objects_b2: u32,
    pub count_arrays_b2: u32,
    pub unk16: u32,
    /// Byte offset where the compressed payload (first zstd/LZ4 frame) begins.
    pub payload_offset: usize,
}

/// The decompressed KV3 payload: the two value buffers plus the (decompressed)
/// binary blobs, ready for the value-tree walk.
pub struct Kv3 {
    pub header: Kv3Header,
    pub buffer1: Vec<u8>,
    pub buffer2: Vec<u8>,
    pub blobs: Vec<u8>,
}

fn parse_header(d: &[u8]) -> Option<Kv3Header> {
    if d.len() < 24 || d[0..4] != KV3_V5_MAGIC {
        return None;
    }
    let mut h = Kv3Header::default();
    let mut o = 4 + 16; // magic + 16-byte format GUID
    macro_rules! u32f { () => {{ let v = le_u32(d, o); o += 4; v }} }
    h.compression_method = u32f!();
    h.compression_dictionary_id = le_u16(d, o); o += 2;
    h.compression_frame_size = le_u16(d, o); o += 2;
    h.count_bytes1 = u32f!();
    h.count_bytes4 = u32f!();
    h.count_bytes8 = u32f!();
    h.count_types = u32f!();
    h.count_objects = le_u16(d, o); o += 2;
    h.count_arrays = le_u16(d, o); o += 2;
    h.size_uncompressed_total = u32f!();
    h.size_compressed_total = u32f!();
    h.count_blocks = u32f!();
    h.size_binary_blobs = u32f!();
    h.count_bytes2 = u32f!();
    h.size_block_compressed_sizes = u32f!();
    h.size_uncompressed_buffer1 = u32f!();
    h.size_compressed_buffer1 = u32f!();
    h.size_uncompressed_buffer2 = u32f!();
    h.size_compressed_buffer2 = u32f!();
    h.count_bytes1_b2 = u32f!();
    h.count_bytes2_b2 = u32f!();
    h.count_bytes4_b2 = u32f!();
    h.count_bytes8_b2 = u32f!();
    h.unk13 = u32f!();
    h.count_objects_b2 = u32f!();
    h.count_arrays_b2 = u32f!();
    h.unk16 = u32f!();
    if o > d.len() {
        return None;
    }
    h.payload_offset = o;
    Some(h)
}

/// Inflate one zstd frame from `src` to `expected` bytes.
fn zstd(src: &[u8], expected: usize) -> Option<Vec<u8>> {
    let mut dec = ruzstd::StreamingDecoder::new(src).ok()?;
    let mut out = Vec::with_capacity(expected);
    dec.read_to_end(&mut out).ok()?;
    Some(out)
}

impl Kv3 {
    /// Parse + decompress a KV3 v5 block. Only compression methods 0 (none) and
    /// 2 (zstd) are handled — what CS2 map PHYS/DATA blocks actually use.
    pub fn parse(block: &[u8]) -> Option<Kv3> {
        let h = parse_header(block)?;
        let mut o = h.payload_offset;

        let (buffer1, buffer2, blobs) = match h.compression_method {
            0 => {
                // Buffers stored raw, back to back, then raw blobs.
                let b1_end = o + h.size_uncompressed_buffer1 as usize;
                let b2_end = b1_end + h.size_uncompressed_buffer2 as usize;
                let blob_end = b2_end + h.size_binary_blobs as usize;
                if blob_end > block.len() {
                    return None;
                }
                (
                    block[o..b1_end].to_vec(),
                    block[b1_end..b2_end].to_vec(),
                    block[b2_end..blob_end.min(block.len())].to_vec(),
                )
            }
            2 => {
                // Each buffer is its own zstd frame; blobs follow as further
                // frame(s). Frame boundaries were confirmed exact against the
                // header's compressed-size fields.
                let b1c_end = o + h.size_compressed_buffer1 as usize;
                let buffer1 = zstd(block.get(o..b1c_end)?, h.size_uncompressed_buffer1 as usize)?;
                o = b1c_end;
                let b2c_end = o + h.size_compressed_buffer2 as usize;
                let buffer2 = zstd(block.get(o..b2c_end)?, h.size_uncompressed_buffer2 as usize)?;
                o = b2c_end;
                // Remaining bytes are the compressed binary-blob frame(s). When
                // there are no blobs this slice is empty.
                let blobs = if h.size_binary_blobs > 0 && o < block.len() {
                    zstd(&block[o..], h.size_binary_blobs as usize).unwrap_or_default()
                } else {
                    Vec::new()
                };
                (buffer1, buffer2, blobs)
            }
            _ => return None, // LZ4 (1) not needed for the CS2 map path yet
        };

        Some(Kv3 { header: h, buffer1, buffer2, blobs })
    }

    /// Decode the value tree. Returns the root value (an Object for a resource
    /// data/PHYS block).
    pub fn root(&self) -> Option<Value> {
        // The root reads its own type (an OBJECT) then its value, like any node.
        let mut r = Reader::new(self)?;
        let t = r.read_type()?;
        r.read_value(t, 0)
    }
}

// ─── Value tree ──────────────────────────────────────────────────────────────

/// A decoded KV3 value. Numbers are normalised to i64/f64; binary blobs (the
/// mesh vertex/index payloads) are kept as raw bytes.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    String(String),
    Blob(Vec<u8>),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    /// Look up a member of an Object by key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        match self { Value::Array(a) => Some(a), _ => None }
    }
    pub fn as_blob(&self) -> Option<&[u8]> {
        match self { Value::Blob(b) => Some(b), _ => None }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self { Value::Int(i) => Some(*i), Value::Double(d) => Some(*d as i64), _ => None }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self { Value::Double(d) => Some(*d), Value::Int(i) => Some(*i as f64), _ => None }
    }
}

// KV3 binary node types. Values quoted verbatim from ValveResourceFormat's
// BinaryKV3.NodeType.cs (the enum is 1-based).
const T_NULL: u8 = 1;
const T_BOOL: u8 = 2;
const T_INT64: u8 = 3;
const T_UINT64: u8 = 4;
const T_DOUBLE: u8 = 5;
const T_STRING: u8 = 6;
const T_BLOB: u8 = 7;
const T_ARRAY: u8 = 8;
const T_OBJECT: u8 = 9;
const T_ARRAY_TYPED: u8 = 10;
const T_INT32: u8 = 11;
const T_UINT32: u8 = 12;
const T_BOOL_TRUE: u8 = 13;
const T_BOOL_FALSE: u8 = 14;
const T_INT64_ZERO: u8 = 15;
const T_INT64_ONE: u8 = 16;
const T_DOUBLE_ZERO: u8 = 17;
const T_DOUBLE_ONE: u8 = 18;
const T_FLOAT: u8 = 19;
const T_INT16: u8 = 20;
const T_UINT16: u8 = 21;
const T_INT32_AS_BYTE: u8 = 23;
const T_ARRAY_BYTE_LEN: u8 = 24;
const T_ARRAY_AUX: u8 = 25;

const MAX_DEPTH: u32 = 200;

/// A position-tracking view over one decompressed byte stream.
struct Stream<'a> { d: &'a [u8], p: usize }
impl<'a> Stream<'a> {
    fn new(d: &'a [u8]) -> Self { Stream { d, p: 0 } }
    fn u8(&mut self) -> Option<u8> { let v = *self.d.get(self.p)?; self.p += 1; Some(v) }
    fn u16(&mut self) -> Option<u16> {
        let v = u16::from_le_bytes(self.d.get(self.p..self.p + 2)?.try_into().ok()?);
        self.p += 2; Some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_le_bytes(self.d.get(self.p..self.p + 4)?.try_into().ok()?);
        self.p += 4; Some(v)
    }
    fn i32(&mut self) -> Option<i32> { self.u32().map(|v| v as i32) }
    fn u64(&mut self) -> Option<u64> {
        let v = u64::from_le_bytes(self.d.get(self.p..self.p + 8)?.try_into().ok()?);
        self.p += 8; Some(v)
    }
    fn f32(&mut self) -> Option<f32> { self.u32().map(f32::from_bits) }
    fn f64(&mut self) -> Option<f64> { self.u64().map(f64::from_bits) }
}

/// The four numeric value streams of one buffer (swapped wholesale when a typed
/// array switches to the auxiliary buffer).
struct BufCtx<'a> { b1: Stream<'a>, b2: Stream<'a>, b4: Stream<'a>, b8: Stream<'a> }

fn align(x: usize, a: usize) -> usize { (x + (a - 1)) & !(a - 1) }

/// Carve a buffer into its (bytes1, bytes2, bytes4, bytes8) sub-streams given
/// the element counts, with the 2/4/8-byte alignment between them. Returns the
/// four slices plus the offset just past bytes8 (where types/strings follow).
fn split_buffer<'a>(
    buf: &'a [u8], c1: usize, c2: usize, c4: usize, c8: usize, start: usize,
) -> Option<(&'a [u8], &'a [u8], &'a [u8], &'a [u8], usize)> {
    let mut o = start;
    let s1 = buf.get(o..o + c1)?; o += c1;
    o = align(o, 2);
    let s2 = buf.get(o..o + c2 * 2)?; o += c2 * 2;
    o = align(o, 4);
    let s4 = buf.get(o..o + c4 * 4)?; o += c4 * 4;
    o = align(o, 8);
    let s8 = buf.get(o..o + c8 * 8)?; o += c8 * 8;
    Some((s1, s2, s4, s8, o))
}

struct Reader<'a> {
    active: BufCtx<'a>,
    aux: BufCtx<'a>,
    types: Stream<'a>,
    obj_lengths: Stream<'a>,
    blob_lengths: Stream<'a>,
    blobs: Stream<'a>,
    strings: Vec<String>,
}

impl<'a> Reader<'a> {
    fn new(kv: &'a Kv3) -> Option<Reader<'a>> {
        let h = &kv.header;
        // Auxiliary = buffer1: bytes1 (holds the string table at its front),
        // bytes2, bytes4 (starts with the string count), bytes8.
        let (x1, x2, x4, x8, _) = split_buffer(
            &kv.buffer1, h.count_bytes1 as usize, h.count_bytes2 as usize,
            h.count_bytes4 as usize, h.count_bytes8 as usize, 0,
        )?;
        // String count is the first int32 of buffer1's bytes4; the strings
        // themselves are NUL-terminated runs at the front of buffer1's bytes1.
        let str_count = u32::from_le_bytes(x4.get(0..4)?.try_into().ok()?) as usize;
        let mut strings = Vec::with_capacity(str_count);
        let mut sp = 0usize;
        for _ in 0..str_count {
            let end = x1[sp..].iter().position(|&b| b == 0)? + sp;
            strings.push(String::from_utf8_lossy(&x1[sp..end]).into_owned());
            sp = end + 1;
        }
        let mut aux = BufCtx {
            b1: Stream::new(x1), b2: Stream::new(x2), b4: Stream::new(x4), b8: Stream::new(x8),
        };
        aux.b1.p = sp;     // byte-values follow the strings
        aux.b4.p = 4;      // first int32 was the string count

        // Active = buffer2 (the value buffer for v5), laid out: object-lengths,
        // bytes1/2/4/8, then the TYPES stream, then the binary-blob lengths.
        // (Order quoted verbatim from ValveResourceFormat's v5 path.)
        let obj_len_bytes = h.count_objects_b2 as usize * 4;
        let (a1, a2, a4, a8, after8) = split_buffer(
            &kv.buffer2, h.count_bytes1_b2 as usize, h.count_bytes2_b2 as usize,
            h.count_bytes4_b2 as usize, h.count_bytes8_b2 as usize, obj_len_bytes,
        )?;
        let types = kv.buffer2.get(after8..after8 + h.count_types as usize)?;
        let blob_len_start = after8 + h.count_types as usize;
        let blob_lengths = kv.buffer2
            .get(blob_len_start..blob_len_start + h.count_blocks as usize * 4)
            .unwrap_or(&[]);

        Some(Reader {
            active: BufCtx {
                b1: Stream::new(a1), b2: Stream::new(a2), b4: Stream::new(a4), b8: Stream::new(a8),
            },
            aux,
            types: Stream::new(types),
            obj_lengths: Stream::new(&kv.buffer2[0..obj_len_bytes]),
            blob_lengths: Stream::new(blob_lengths),
            blobs: Stream::new(&kv.blobs),
            strings,
        })
    }

    /// Read a type byte (+ optional flag byte), returning the bare node type.
    fn read_type(&mut self) -> Option<u8> {
        let mut t = self.types.u8()?;
        if t & 0x80 != 0 {
            t &= 0x3F;
            let _flag = self.types.u8()?; // string/resource hint — unused here
        }
        Some(t)
    }

    fn string_by_id(&self, id: i32) -> String {
        if id < 0 { String::new() } else { self.strings.get(id as usize).cloned().unwrap_or_default() }
    }

    /// Parse one node, mirroring ValveResourceFormat's ParseBinaryKV3: read the
    /// TYPE first; in an object also read the field-name id (from the active
    /// bytes4) before the value; in an array read only the value.
    fn parse_node(&mut self, in_array: bool, depth: u32) -> Option<(String, Value)> {
        let t = self.read_type()?;
        if in_array {
            return Some((String::new(), self.read_value(t, depth)?));
        }
        let id = self.active.b4.i32()?;
        let name = self.string_by_id(id);
        let v = self.read_value(t, depth)?;
        Some((name, v))
    }

    /// Read `count` elements all of node type `t` (typed-array fast path).
    fn read_typed_elems(&mut self, t: u8, count: usize, depth: u32) -> Option<Vec<Value>> {
        let mut out = Vec::with_capacity(count.min(1 << 20));
        for _ in 0..count { out.push(self.read_value(t, depth + 1)?); }
        Some(out)
    }

    fn read_value(&mut self, t: u8, depth: u32) -> Option<Value> {
        if depth > MAX_DEPTH { return None; }
        Some(match t {
            T_NULL => Value::Null,
            T_BOOL_TRUE => Value::Bool(true),
            T_BOOL_FALSE => Value::Bool(false),
            T_INT64_ZERO => Value::Int(0),
            T_INT64_ONE => Value::Int(1),
            T_DOUBLE_ZERO => Value::Double(0.0),
            T_DOUBLE_ONE => Value::Double(1.0),
            T_BOOL => Value::Bool(self.active.b1.u8()? != 0),
            T_INT32_AS_BYTE => Value::Int(self.active.b1.u8()? as i64),
            T_INT16 => Value::Int(self.active.b2.u16()? as i16 as i64),
            T_UINT16 => Value::Int(self.active.b2.u16()? as i64),
            T_INT32 => Value::Int(self.active.b4.i32()? as i64),
            T_UINT32 => Value::Int(self.active.b4.u32()? as i64),
            T_FLOAT => Value::Double(self.active.b4.f32()? as f64),
            T_INT64 => Value::Int(self.active.b8.u64()? as i64),
            T_UINT64 => Value::Int(self.active.b8.u64()? as i64),
            T_DOUBLE => Value::Double(self.active.b8.f64()?),
            T_STRING => { let id = self.active.b4.i32()?; Value::String(self.string_by_id(id)) }
            T_BLOB => {
                let len = self.blob_lengths.u32()? as usize;
                let start = self.blobs.p;
                let end = start + len;
                let b = self.blobs.d.get(start..end)?.to_vec();
                self.blobs.p = end;
                Value::Blob(b)
            }
            T_ARRAY => {
                let n = self.active.b4.u32()? as usize;
                let mut out = Vec::with_capacity(n.min(1 << 20));
                for _ in 0..n {
                    out.push(self.parse_node(true, depth + 1)?.1);
                }
                Value::Array(out)
            }
            T_ARRAY_TYPED => {
                let n = self.active.b4.u32()? as usize;
                let et = self.read_type()?;
                Value::Array(self.read_typed_elems(et, n, depth)?)
            }
            T_ARRAY_BYTE_LEN => {
                let n = self.active.b1.u8()? as usize;
                let et = self.read_type()?;
                Value::Array(self.read_typed_elems(et, n, depth)?)
            }
            T_ARRAY_AUX => {
                // A typed array whose elements live in the auxiliary buffer. The
                // length is a single byte from the *active* buffer (read before
                // the swap); the subtype is read from the shared types stream;
                // only the element data comes from the swapped-in buffer.
                let n = self.active.b1.u8()? as usize;
                let et = self.read_type()?;
                std::mem::swap(&mut self.active, &mut self.aux);
                let elems = self.read_typed_elems(et, n, depth);
                std::mem::swap(&mut self.active, &mut self.aux);
                Value::Array(elems?)
            }
            T_OBJECT => {
                let n = self.obj_lengths.u32()? as usize;
                let mut out = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    out.push(self.parse_node(false, depth + 1)?);
                }
                Value::Object(out)
            }
            _ => return None, // unknown / unsupported node type
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{resource::Resource, vpk::Vpk};

    // End-to-end foundation check against a real CS2 map pak. Self-skips unless
    // CS2_VPK points at one, e.g.:
    //   CS2_VPK="DEMOS TESTING/de_nuke.vpk" cargo test --lib cs2_map_foundation -- --nocapture
    #[test]
    fn cs2_map_foundation() {
        let path = match std::env::var("CS2_VPK") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("[skip] set CS2_VPK to a CS2 map .vpk to run");
                return;
            }
        };
        let data = std::fs::read(&path).expect("read CS2_VPK");
        let vpk = Vpk::parse(&data).expect("parse VPK");
        eprintln!("VPK entries: {}", vpk.entries.len());

        let entry = vpk
            .find("world_physics.vmdl_c")
            .expect("world_physics.vmdl_c present");
        let bytes = vpk.read(entry).expect("read world_physics");
        let res = Resource::parse(&bytes).expect("parse resource");
        let phys = res.block(b"PHYS").expect("PHYS block present");

        let kv = Kv3::parse(phys).expect("parse + decompress KV3 PHYS");
        eprintln!(
            "PHYS KV3: method={} buf1={} buf2={} blobs={} (expected b1={} b2={} blobs={})",
            kv.header.compression_method,
            kv.buffer1.len(),
            kv.buffer2.len(),
            kv.blobs.len(),
            kv.header.size_uncompressed_buffer1,
            kv.header.size_uncompressed_buffer2,
            kv.header.size_binary_blobs,
        );
        assert_eq!(kv.buffer1.len(), kv.header.size_uncompressed_buffer1 as usize);
        assert_eq!(kv.buffer2.len(), kv.header.size_uncompressed_buffer2 as usize);
        assert_eq!(kv.blobs.len(), kv.header.size_binary_blobs as usize);

        // Decode the value tree: the root is the physics aggregate object and
        // exposes the world collision shapes.
        let root = kv.root().expect("decode KV3 value tree");
        let parts = root.get("m_parts").and_then(|v| v.as_array()).expect("m_parts array");
        assert!(!parts.is_empty(), "expected at least one physics part");
        let shape = parts[0].get("m_rnShape").expect("m_rnShape");
        let n_hulls = shape.get("m_hulls").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        let n_meshes = shape.get("m_meshes").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        assert!(n_hulls + n_meshes > 0, "expected hull/mesh collision shapes");

        // Walk the shapes into one triangle mesh and sanity-check its extent.
        let (verts, indices) = super::super::vphys::extract_collision_mesh(&root)
            .expect("extract collision mesh");
        let mut mn = [f32::MAX; 3];
        let mut mx = [f32::MIN; 3];
        for v in &verts {
            for k in 0..3 { mn[k] = mn[k].min(v[k]); mx[k] = mx[k].max(v[k]); }
        }
        eprintln!(
            "de_nuke collision: {} hulls + {} meshes -> {} verts, {} triangles | \
             bounds X[{:.0},{:.0}] Y[{:.0},{:.0}] Z[{:.0},{:.0}]",
            n_hulls, n_meshes, verts.len(), indices.len() / 3,
            mn[0], mx[0], mn[1], mx[1], mn[2], mx[2],
        );
        assert!(verts.len() > 1000 && indices.len() > 3000, "expected substantial geometry");
        // Map-sane extent (units): de_nuke spans several thousand units per axis.
        assert!((mx[0] - mn[0]) > 2000.0 && (mx[1] - mn[1]) > 2000.0, "geometry extent too small");
    }
}
