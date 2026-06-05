// A tiny, dependency-free protobuf *wire-format* reader.
//
// CS:GO (and every later Source / Source 2 title) wraps its net messages in
// protobuf — `CSVCMsg_PacketEntities`, `CSVCMsg_SendTable`, … — instead of the
// raw bit-packed `svc_*` framing the older engines use. To decode those we only
// need to walk the protobuf *wire format*; we do **not** need generated message
// structs, a schema compiler, or the `prost`/`protobuf` crates (which would
// break demoscope's zero-dependency, single-binary ethos).
//
// This module implements just the wire format from
// <https://protobuf.dev/programming-guides/encoding/>: tag-prefixed fields in
// one of four wire types (varint / 64-bit / length-delimited / 32-bit). Callers
// read a message field-by-field with `Reader::next_field` and pull typed values
// off each `Field` via the `Value` helpers. Mapping field *numbers* to message
// semantics (e.g. "field 1 of CSVCMsg_SendTable is `is_end`") lives with the
// consumer in `source_demo`, not here — this layer stays schema-agnostic.
//
// Deliberately omitted (add only when a consumer needs them): groups (wire
// types 3/4, deprecated since proto2) and packed-repeated *iteration* helpers
// beyond handing back the raw length-delimited slice.

mod reader;

pub use reader::{Field, Reader, Value, WireType};

/// Errors from walking a malformed or truncated protobuf buffer. Decoders turn
/// these into a skipped message rather than aborting the whole demo, so the
/// variants stay coarse — enough to log, not enough to need matching on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Ran off the end of the buffer mid-field.
    Truncated,
    /// A varint spanned more than 10 bytes (can't fit in a u64).
    VarintOverflow,
    /// Tag carried wire type 3 or 4 (start/end group) — unsupported.
    UnsupportedWireType(u8),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Truncated => write!(f, "protobuf buffer truncated"),
            Error::VarintOverflow => write!(f, "protobuf varint exceeds 64 bits"),
            Error::UnsupportedWireType(w) => write!(f, "unsupported protobuf wire type {w}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
