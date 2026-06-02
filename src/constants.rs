// Format constants shared across the demo walkers: container magics, the demo
// command-byte enum (with its proto-3 vs proto-4 split), and the IN_* button
// masks. Kept dependency-free so every other module can pull from here.

pub(crate) const DEMO_MAGIC: &[u8; 8] = b"HL2DEMO\0";
// Garry's Mod 13+ records the byte-for-byte identical HL2DEMO container (same
// 1072-byte header layout, same proto-3 packet stream) but stamps it with a
// renamed 8-byte magic. Accept it as a Source demo - the only thing that
// actually differs is these 8 bytes. (Entity tracks are a separate matter:
// GMod reports demo_protocol=3 but its engine networks entities in the newer
// proto-4 style, so the proto-3 entity decoder still yields 0 tracks - inputs,
// events, names, and the recorder camera path all decode fine.)
pub(crate) const GMOD_MAGIC: &[u8; 8] = b"GMODEMO\0";
pub(crate) const HEADER_SIZE: usize = 1072;

/// True if the leading bytes are a Source `HL2DEMO` container - either the
/// canonical magic or Garry's Mod's renamed-but-identical `GMODEMO`.
pub(crate) fn is_source_demo_magic(data: &[u8]) -> bool {
    data.len() >= 8 && (&data[0..8] == DEMO_MAGIC || &data[0..8] == GMOD_MAGIC)
}
// Size of one democmdinfo Split_t. The full block is SPLIT_SIZE × splitscreen
// slots - see detect_splitscreen() (L4D ships 4 slots, not 1).
pub(crate) const SPLIT_SIZE: usize = 76; // flags(4) + 6 × vec3(12) = 76

// Packet command IDs
pub(crate) const DEM_SIGNON: u8 = 1;
pub(crate) const DEM_PACKET: u8 = 2;
pub(crate) const DEM_SYNCTICK: u8 = 3;
pub(crate) const DEM_CONSOLECMD: u8 = 4;
pub(crate) const DEM_USERCMD: u8 = 5;
pub(crate) const DEM_DATATABLES: u8 = 6;
pub(crate) const DEM_STOP: u8 = 7;
// Two demo command enums share demo_protocol 4. Orange Box/Portal 2 put
// StringTables at 8. L4D/CS:GO inserted CustomData at 8 and pushed StringTables
// to 9. Both StringTables variants are length-prefixed, so we walk them the
// same way; the only thing that moves is the command id.
pub(crate) const DEM_STRINGTABLES: u8 = 8;
pub(crate) const DEM_STRINGTABLES_V2: u8 = 9; // L4D/CS:GO (CustomData took slot 8)

// MAX_EDICT_BITS / WEAPON_SUBTYPE_BITS from Source SDK
pub(crate) const MAX_EDICT_BITS: u32 = 11;
pub(crate) const WEAPON_SUBTYPE_BITS: u32 = 6;

// IN_* button masks
pub(crate) const IN_ATTACK: u32 = 1 << 0;
pub(crate) const IN_JUMP: u32 = 1 << 1;
pub(crate) const IN_DUCK: u32 = 1 << 2;
pub(crate) const IN_FORWARD: u32 = 1 << 3;
pub(crate) const IN_BACK: u32 = 1 << 4;
pub(crate) const IN_USE: u32 = 1 << 5;
pub(crate) const IN_LEFT: u32 = 1 << 7;
pub(crate) const IN_RIGHT: u32 = 1 << 8;
pub(crate) const IN_MOVELEFT: u32 = 1 << 9;
pub(crate) const IN_MOVERIGHT: u32 = 1 << 10;
pub(crate) const IN_ATTACK2: u32 = 1 << 11;
pub(crate) const IN_RELOAD: u32 = 1 << 13;
pub(crate) const IN_SCORE: u32 = 1 << 16;
pub(crate) const IN_SPEED: u32 = 1 << 17;
pub(crate) const IN_WALK: u32 = 1 << 18;
pub(crate) const IN_ZOOM: u32 = 1 << 19;
