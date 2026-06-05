// CS:GO (Source 1, demo_protocol 4, net_protocol ~13xxx) message routing.
//
// CS:GO keeps the same *demo container* as the rest of the proto-4 family
// (HL2DEMO header, DEM_* commands, 2-slot splitscreen democmdinfo) — that part
// is already handled in `player_tracks.rs`. What changed is the *net-message*
// framing *inside* DEM_PACKET / DEM_DATATABLES: older engines bit-pack a 6-bit
// message id + a hand-rolled struct; CS:GO replaced that with **protobuf**.
//
// In a CS:GO DEM_PACKET payload the messages are laid out as a flat sequence of
//   varint(msg_type)  varint(size)  <size bytes of protobuf body>
// repeated until the payload is consumed. Each body is a `CNETMsg_*` / `CSVCMsg_*`
// message we decode with the schema-agnostic `protobuf::Reader` (see `src/protobuf`).
//
// This module is the routing/identification layer. It currently enumerates and
// frames the messages (Phase 1, verified against real CS:GO demos); the
// SendTable → flatten → PacketEntities decode that turns them into position
// tracks builds on top of this and the existing `datatable`/`packetentities`
// decoders.
#![allow(dead_code)]

pub mod entities;
pub mod events;
pub mod message;
pub mod sendtable;
pub mod stringtables;

pub use message::{scan_payload, MsgKind, NetMessage};
