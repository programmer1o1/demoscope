// Demo header + UserCmd: the fixed 1072-byte header struct and the bit-packed
// CUserCmd payload, plus their parsers and the button-name formatter.

use super::bitreader::BitReader;
use super::bytes::{le_f32, le_i32, read_cstring};
use super::constants::{
    is_source_demo_magic, HEADER_SIZE, IN_ATTACK, IN_ATTACK2, IN_BACK, IN_DUCK, IN_FORWARD,
    IN_JUMP, IN_LEFT, IN_MOVELEFT, IN_MOVERIGHT, IN_RELOAD, IN_RIGHT, IN_SCORE, IN_SPEED, IN_USE,
    IN_WALK, IN_ZOOM, MAX_EDICT_BITS, WEAPON_SUBTYPE_BITS,
};

#[derive(Debug)]
pub(crate) struct DemoHeader {
    pub(crate) demo_protocol: i32,
    pub(crate) net_protocol: i32,
    pub(crate) server_name: String,
    pub(crate) client_name: String,
    pub(crate) map_name: String,
    pub(crate) game_dir: String,
    pub(crate) playback_time: f32,
    pub(crate) ticks: i32,
    pub(crate) frames: i32,
    pub(crate) sign_on_length: i32,
}

#[derive(Debug, Default)]
pub(crate) struct UserCmd {
    pub(crate) command_number: Option<u32>,
    pub(crate) tick_count: Option<u32>,
    pub(crate) pitch: Option<f32>,
    pub(crate) yaw: Option<f32>,
    pub(crate) roll: Option<f32>,
    pub(crate) forwardmove: Option<f32>,
    pub(crate) sidemove: Option<f32>,
    pub(crate) upmove: Option<f32>,
    pub(crate) buttons: Option<u32>,
    pub(crate) impulse: Option<u8>,
    pub(crate) weaponselect: Option<u32>,
    pub(crate) weaponsubtype: Option<u32>,
    pub(crate) mousedx: Option<i16>,
    pub(crate) mousedy: Option<i16>,
}

pub(crate) fn parse_header(data: &[u8]) -> Option<DemoHeader> {
    if data.len() < HEADER_SIZE {
        return None;
    }
    if !is_source_demo_magic(data) {
        return None;
    }
    Some(DemoHeader {
        demo_protocol: le_i32(data, 8),
        net_protocol: le_i32(data, 12),
        server_name: read_cstring(data, 16, 260),
        client_name: read_cstring(data, 276, 260),
        map_name: read_cstring(data, 536, 260),
        game_dir: read_cstring(data, 796, 260),
        playback_time: le_f32(data, 1056),
        ticks: le_i32(data, 1060),
        frames: le_i32(data, 1064),
        sign_on_length: le_i32(data, 1068),
    })
}

// Parses a CUserCmd payload using Source Engine ReadUsercmd() format.
// Each field is preceded by a 1-bit "has this field" flag in the CBitBuf stream.
// Returns partial ucmd on buffer exhaustion rather than failing entirely.
pub(crate) fn parse_usercmd(data: &[u8]) -> Option<UserCmd> {
    let mut br = BitReader::new(data);
    let mut ucmd = UserCmd::default();

    macro_rules! has {
        () => {
            match br.read_bits(1) {
                Some(v) => v != 0,
                None => return Some(ucmd),
            }
        };
    }

    if has!() { ucmd.command_number = br.read_u32(); }
    if has!() { ucmd.tick_count    = br.read_u32(); }
    if has!() { ucmd.pitch          = br.read_bit_float(); }
    if has!() { ucmd.yaw            = br.read_bit_float(); }
    if has!() { ucmd.roll           = br.read_bit_float(); }
    if has!() { ucmd.forwardmove    = br.read_bit_float(); }
    if has!() { ucmd.sidemove       = br.read_bit_float(); }
    if has!() { ucmd.upmove         = br.read_bit_float(); }
    if has!() { ucmd.buttons        = br.read_u32(); }
    if has!() { ucmd.impulse        = br.read_byte(); }
    if has!() {
        ucmd.weaponselect = br.read_bits(MAX_EDICT_BITS);
        if has!() { ucmd.weaponsubtype = br.read_bits(WEAPON_SUBTYPE_BITS); }
    }
    if has!() { ucmd.mousedx = br.read_i16(); }
    if has!() { ucmd.mousedy = br.read_i16(); }

    Some(ucmd)
}

pub(crate) fn fmt_buttons(b: u32) -> String {
    const NAMES: &[(u32, &str)] = &[
        (IN_ATTACK, "ATTACK"),
        (IN_ATTACK2, "ATTACK2"),
        (IN_JUMP, "JUMP"),
        (IN_DUCK, "DUCK"),
        (IN_FORWARD, "FORWARD"),
        (IN_BACK, "BACK"),
        (IN_MOVELEFT, "MOVELEFT"),
        (IN_MOVERIGHT, "MOVERIGHT"),
        (IN_USE, "USE"),
        (IN_RELOAD, "RELOAD"),
        (IN_SCORE, "SCORE"),
        (IN_SPEED, "SPEED"),
        (IN_WALK, "WALK"),
        (IN_ZOOM, "ZOOM"),
        (IN_LEFT, "TURNLEFT"),
        (IN_RIGHT, "TURNRIGHT"),
    ];
    let active: Vec<&str> = NAMES
        .iter()
        .filter(|(f, _)| b & f != 0)
        .map(|(_, n)| *n)
        .collect();
    if active.is_empty() {
        "none".into()
    } else {
        active.join("|")
    }
}
