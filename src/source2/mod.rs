// Source 2 (`PBDEMS2`) demo decoder — positions-only slice.
//
// CS2 / Dota 2 / Deadlock ship the Source 2 demo container (`PBDEMS2\0\0`),
// which shares NOTHING with the HL2DEMO entity pipeline beyond the protobuf
// wire format. This module decodes the *minimum* needed to emit per-player
// position/angle tracks for the viewer — it is deliberately NOT a general
// Source 2 parser. Full prop decode (weapons, life state, etc.) and Source 2
// map overlays are out of scope (see ROADMAP.md).
//
// Pipeline (each stage is a sibling module):
//   container  — PBDEMS2 magic + outer frame walk (varint cmd/tick/size,
//                snappy bit on the command). Routes CDemo* messages.
//   snappy     — zero-dependency snappy block decompressor for frame bodies.
//   serializer — CSVCMsg_FlattenedSerializer, TRIMMED to the few fields we
//                need (cell coords + origin + angles); everything else dropped.
//   fieldpath  — the ~40-op field-path Huffman tree used to walk entity deltas.
//   decoders   — the three float decoders players use (quantized / coord /
//                normal) plus the cell→world coordinate reconstruction.
//   entities   — instancebaseline bootstrap + CSVCMsg_PacketEntities delta walk,
//                accumulating per-entity origin[*]/angles[*].
//
// What is REUSED from the Source 1 path: the protobuf wire reader
// (`crate::protobuf`) and the final `MultiPlayerData` → viewer output. What is
// built fresh lives entirely under this folder so no other engine is touched.
#![allow(dead_code)]

pub mod bitreader;
pub mod container;
pub mod entities;
pub mod fieldpath;
pub mod kv3;
pub mod map;
pub mod parser;
pub mod quantizedfloat;
pub mod resource;
pub mod serializer;
pub mod snappy;
pub mod stringtable;
pub mod vphys;
pub mod vpk;

/// Source 2 demo container magic: the 7 ASCII bytes `PBDEMS2` plus a NUL
/// terminator (8 bytes), followed by two i32 offsets (the full header is 16 B).
pub const PBDEMS2_MAGIC: &[u8] = b"PBDEMS2\0";

/// True if `data` looks like a Source 2 (`PBDEMS2`) demo. Checked AFTER the
/// HL2DEMO / HLDEMO / Quake routes so a Source 1 demo is never misclassified.
pub fn is_source2(data: &[u8]) -> bool {
    data.len() >= PBDEMS2_MAGIC.len() && &data[..PBDEMS2_MAGIC.len()] == PBDEMS2_MAGIC
}

pub use container::{parse_meta, Source2Meta};
pub use parser::{parse, Source2Tracks};
