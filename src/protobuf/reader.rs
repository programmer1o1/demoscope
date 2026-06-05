// The protobuf wire-format cursor. See `mod.rs` for the why.
//
// Wire format recap (protobuf.dev/programming-guides/encoding):
//   * A message is a flat sequence of fields, each prefixed by a varint *tag*.
//   * tag = (field_number << 3) | wire_type.
//   * Varints are little-endian base-128: 7 payload bits per byte, the high bit
//     (0x80) is a "more bytes follow" continuation flag.
//   * Four live wire types:
//       0 Varint           int32/64, uint32/64, sint*, bool, enum
//       1 Fixed64          fixed64, sfixed64, double
//       2 Len              string, bytes, embedded message, packed repeated
//       5 Fixed32          fixed32, sfixed32, float
//   * Fixed widths are little-endian; signed varints (`sint*`) are zig-zagged.

use super::{Error, Result};

/// The four wire types we support (groups, 3/4, are rejected at the tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    Varint,
    Fixed64,
    Len,
    Fixed32,
}

impl WireType {
    fn from_tag(w: u8) -> Result<WireType> {
        match w {
            0 => Ok(WireType::Varint),
            1 => Ok(WireType::Fixed64),
            2 => Ok(WireType::Len),
            5 => Ok(WireType::Fixed32),
            other => Err(Error::UnsupportedWireType(other)),
        }
    }
}

/// One field's raw value, still in wire form. Typed accessors below interpret
/// it; an accessor that doesn't fit the stored wire type returns `None` so a
/// schema mismatch degrades gracefully instead of panicking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Len(&'a [u8]),
    Fixed32(u32),
}

impl<'a> Value<'a> {
    /// Raw unsigned varint (also the storage for bool/enum/int*). For `Fixed*`
    /// returns the stored bits widened, so callers that know the field is an
    /// integer don't have to match on wire type.
    pub fn as_u64(self) -> Option<u64> {
        match self {
            Value::Varint(v) => Some(v),
            Value::Fixed64(v) => Some(v),
            Value::Fixed32(v) => Some(v as u64),
            Value::Len(_) => None,
        }
    }

    /// Two's-complement signed read of a varint (protobuf `int32`/`int64`).
    /// Note: negative `int32` values are encoded as 10-byte varints on the
    /// wire, so we read the full u64 and truncate.
    pub fn as_i64(self) -> Option<i64> {
        self.as_u64().map(|v| v as i64)
    }

    pub fn as_i32(self) -> Option<i32> {
        self.as_u64().map(|v| v as u32 as i32)
    }

    pub fn as_u32(self) -> Option<u32> {
        self.as_u64().map(|v| v as u32)
    }

    pub fn as_bool(self) -> Option<bool> {
        self.as_u64().map(|v| v != 0)
    }

    /// Zig-zag decode (protobuf `sint32`): maps unsigned varints back to signed
    /// so small-magnitude negatives stay short. `(n >> 1) ^ -(n & 1)`.
    pub fn as_sint32(self) -> Option<i32> {
        self.as_u64().map(|v| {
            let v = v as u32;
            ((v >> 1) as i32) ^ -((v & 1) as i32)
        })
    }

    pub fn as_sint64(self) -> Option<i64> {
        self.as_u64()
            .map(|v| ((v >> 1) as i64) ^ -((v & 1) as i64))
    }

    /// IEEE-754 float from a Fixed32 field. `None` for any other wire type.
    pub fn as_f32(self) -> Option<f32> {
        match self {
            Value::Fixed32(bits) => Some(f32::from_bits(bits)),
            _ => None,
        }
    }

    /// IEEE-754 double from a Fixed64 field. `None` for any other wire type.
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Value::Fixed64(bits) => Some(f64::from_bits(bits)),
            _ => None,
        }
    }

    /// Raw bytes of a length-delimited field (string / bytes / embedded msg).
    pub fn as_bytes(self) -> Option<&'a [u8]> {
        match self {
            Value::Len(b) => Some(b),
            _ => None,
        }
    }

    /// Length-delimited field as UTF-8 (lossy). `None` for non-`Len` fields.
    pub fn as_str(self) -> Option<std::borrow::Cow<'a, str>> {
        self.as_bytes().map(String::from_utf8_lossy)
    }

    /// Length-delimited field reinterpreted as a nested message reader.
    pub fn as_message(self) -> Option<Reader<'a>> {
        self.as_bytes().map(Reader::new)
    }
}

/// One decoded field: its number plus its raw value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Field<'a> {
    pub number: u32,
    pub value: Value<'a>,
}

impl<'a> Field<'a> {
    /// Shorthand so call sites read `f.as_u64()` instead of `f.value.as_u64()`.
    pub fn value(&self) -> Value<'a> {
        self.value
    }
}

/// A forward-only cursor over one protobuf message buffer. Construct with
/// `new`, then pull fields with `next_field` until it yields `None`.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    /// True once every field has been consumed.
    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Bytes not yet consumed — handy for length-framed outer envelopes that
    /// read a message header then hand the remainder to another decoder.
    pub fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos.min(self.buf.len())..]
    }

    /// Read a base-128 varint. LSB-first, 7 bits/byte, 0x80 = continue. Caps at
    /// 10 bytes (the most a u64 needs) to reject runaway buffers.
    pub fn read_varint(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = *self.buf.get(self.pos).ok_or(Error::Truncated)?;
            self.pos += 1;
            // The 10th byte of a max u64 only carries 1 payload bit; anything
            // past 63 bits of shift means the encoder lied about the width.
            if shift >= 64 {
                return Err(Error::VarintOverflow);
            }
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    /// Take exactly `n` raw bytes, advancing the cursor. The counterpart to
    /// `read_len` for outer envelopes that carry the length out-of-band — e.g.
    /// the CS:GO `DEM_PACKET` framing, where each message is a varint type, a
    /// varint size, then `size` bytes of protobuf body read with this.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn read_fixed32(&mut self) -> Result<u32> {
        let end = self.pos + 4;
        let slice = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(u32::from_le_bytes(slice.try_into().unwrap()))
    }

    fn read_fixed64(&mut self) -> Result<u64> {
        let end = self.pos + 8;
        let slice = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(u64::from_le_bytes(slice.try_into().unwrap()))
    }

    fn read_len(&mut self) -> Result<&'a [u8]> {
        let len = self.read_varint()? as usize;
        let end = self.pos.checked_add(len).ok_or(Error::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    /// Decode the next field, or `None` at end of message. A malformed tag or
    /// truncated value returns `Err`; the cursor is left at the failure point.
    pub fn next_field(&mut self) -> Result<Option<Field<'a>>> {
        if self.is_empty() {
            return Ok(None);
        }
        let tag = self.read_varint()?;
        let number = (tag >> 3) as u32;
        let wire = WireType::from_tag((tag & 0x07) as u8)?;
        let value = match wire {
            WireType::Varint => Value::Varint(self.read_varint()?),
            WireType::Fixed64 => Value::Fixed64(self.read_fixed64()?),
            WireType::Len => Value::Len(self.read_len()?),
            WireType::Fixed32 => Value::Fixed32(self.read_fixed32()?),
        };
        Ok(Some(Field { number, value }))
    }
}

/// Iterate fields with `for field in &mut reader { … }`. Stops at end of buffer;
/// a decode error also stops iteration (inspect with `next_field` if you need
/// to distinguish clean end from malformed data).
impl<'a> Iterator for Reader<'a> {
    type Item = Field<'a>;

    fn next(&mut self) -> Option<Field<'a>> {
        // A decode error ends iteration (yields None), same as a clean end.
        self.next_field().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: build a tag byte for single-byte field numbers.
    fn tag(field: u32, wire: u8) -> u8 {
        ((field << 3) | wire as u32) as u8
    }

    #[test]
    fn varint_single_and_multibyte() {
        // 1 → 0x01; 300 → 0xAC 0x02 (the canonical protobuf example).
        let mut r = Reader::new(&[0x01]);
        assert_eq!(r.read_varint().unwrap(), 1);

        let mut r = Reader::new(&[0xAC, 0x02]);
        assert_eq!(r.read_varint().unwrap(), 300);

        // Max u64: ten 0xFF-ish bytes.
        let mut r = Reader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]);
        assert_eq!(r.read_varint().unwrap(), u64::MAX);
    }

    #[test]
    fn varint_truncated_and_overflow() {
        // Continuation bit set but no more bytes.
        let mut r = Reader::new(&[0x80]);
        assert_eq!(r.read_varint(), Err(Error::Truncated));

        // 11 continuation bytes → past 64 bits.
        let mut r = Reader::new(&[0xFF; 11]);
        assert_eq!(r.read_varint(), Err(Error::VarintOverflow));
    }

    #[test]
    fn field_varint() {
        // field 1, wire 0, value 150 (0x96 0x01).
        let buf = [tag(1, 0), 0x96, 0x01];
        let mut r = Reader::new(&buf);
        let f = r.next_field().unwrap().unwrap();
        assert_eq!(f.number, 1);
        assert_eq!(f.value.as_u64(), Some(150));
        assert!(r.next_field().unwrap().is_none());
    }

    #[test]
    fn field_length_delimited_string() {
        // field 2, wire 2, "testing" (7 bytes).
        let mut buf = vec![tag(2, 2), 7];
        buf.extend_from_slice(b"testing");
        let mut r = Reader::new(&buf);
        let f = r.next_field().unwrap().unwrap();
        assert_eq!(f.number, 2);
        assert_eq!(f.value.as_str().unwrap(), "testing");
    }

    #[test]
    fn field_fixed32_float_and_fixed64_double() {
        let mut buf = vec![tag(3, 5)];
        buf.extend_from_slice(&1.5f32.to_le_bits_vec());
        buf.push(tag(4, 1));
        buf.extend_from_slice(&(-2.0f64).to_bits().to_le_bytes());
        let mut r = Reader::new(&buf);
        let f1 = r.next_field().unwrap().unwrap();
        assert_eq!(f1.value.as_f32(), Some(1.5));
        let f2 = r.next_field().unwrap().unwrap();
        assert_eq!(f2.value.as_f64(), Some(-2.0));
    }

    #[test]
    fn zigzag_roundtrip() {
        // sint32: 0→0, -1→1, 1→2, -2→3, 2147483647→…
        let cases: &[(i32, u64)] = &[(0, 0), (-1, 1), (1, 2), (-2, 3), (2147483647, 4294967294)];
        for &(want, enc) in cases {
            assert_eq!(Value::Varint(enc).as_sint32(), Some(want));
        }
    }

    #[test]
    fn nested_message_and_iteration() {
        // Outer field 1 (len) wraps inner {field 1 varint = 42}.
        let inner = [tag(1, 0), 42u8];
        let mut buf = vec![tag(1, 2), inner.len() as u8];
        buf.extend_from_slice(&inner);
        let r = Reader::new(&buf);
        let mut count = 0;
        for field in r {
            count += 1;
            let mut nested = field.value.as_message().unwrap();
            let inner_f = nested.next_field().unwrap().unwrap();
            assert_eq!(inner_f.value.as_u64(), Some(42));
        }
        assert_eq!(count, 1);
    }

    #[test]
    fn unsupported_group_wire_type() {
        // wire type 3 (start group) is rejected.
        let buf = [tag(1, 3)];
        let mut r = Reader::new(&buf);
        assert_eq!(r.next_field(), Err(Error::UnsupportedWireType(3)));
    }

    // Tiny shim so the float test reads cleanly without an extra import.
    trait LeBitsVec {
        fn to_le_bits_vec(self) -> Vec<u8>;
    }
    impl LeBitsVec for f32 {
        fn to_le_bits_vec(self) -> Vec<u8> {
            self.to_bits().to_le_bytes().to_vec()
        }
    }
}
