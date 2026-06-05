// Low-level, dependency-free helpers shared across every engine path:
// little-endian byte readers, the bit-level reader, and the demo-format
// constants. These leaf modules import nothing else in the crate.
pub mod bitreader;
pub mod bytes;
pub mod constants;
