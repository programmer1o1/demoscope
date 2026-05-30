// Native Source 1 demo decoder for player-tracking purposes.
//
// We re-implement only what demoscope's multi-player visualiser needs:
//   * DEM_DATATABLES → SendTable definitions + server-class list (datatable.rs)
//   * svc_PacketEntities (msg 26) → per-entity prop state (packetentities.rs)
//   * SendProp value decoding (int / float / vector / arrays) (sendprop.rs)
//   * DEM_STRINGTABLES userinfo → entity_id → player name (stringtable.rs)
//
// We do *not* re-implement: NetMessages we don't care about, game events,
// usercmd parsing (already in the main code), other StringTables.
//
// These submodules keep some intentionally-unused Source-protocol constants
// and BitReader helpers around as documentation (and so the symbols stay
// resolvable when other parts of the code reference them via name). The
// blanket allow keeps `cargo build` quiet.
#![allow(dead_code)]

pub mod bitreader;
pub mod datatable;
pub mod packetentities;
pub mod sendprop;
pub mod stringtable;
pub mod player_tracks;
