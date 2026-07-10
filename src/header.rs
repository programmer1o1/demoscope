// Demo header + UserCmd: the fixed 1072-byte header struct and the bit-packed
// CUserCmd payload, plus their parsers and the button-name formatter.

use super::util::bitreader::BitReader;
use super::util::bytes::{le_f32, le_i32, read_cstring};
use super::util::constants::{
    HEADER_SIZE, IN_ALT1, IN_ALT2, IN_ATTACK, IN_ATTACK2, IN_ATTACK3, IN_BACK, IN_BULLRUSH,
    IN_CANCEL, IN_DUCK, IN_FORWARD, IN_GRENADE1, IN_GRENADE2, IN_JUMP, IN_LEFT, IN_MOVELEFT,
    IN_MOVERIGHT, IN_RELOAD, IN_RIGHT, IN_RUN, IN_SCORE, IN_SPEED, IN_USE, IN_WALK, IN_WEAPON1,
    IN_WEAPON2, IN_ZOOM, MAX_EDICT_BITS, SOURCE_BUTTON_MASK, WEAPON_SUBTYPE_BITS,
    is_source_demo_magic,
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

enum UserCmdAngleMode {
    Float32,
    BitAngle16,
}

// Parses a CUserCmd payload using Source Engine ReadUsercmd() format.
// Each field is preceded by a 1-bit "has this field" flag in the CBitBuf stream.
// Returns partial ucmd on buffer exhaustion rather than failing entirely.
pub(crate) fn parse_usercmd(data: &[u8]) -> Option<UserCmd> {
    parse_usercmd_with_angles(data, UserCmdAngleMode::Float32)
}

/// Parse a CUserCmd using the 2004-era Source layout. The field presence bits
/// match later Source, but view angles were serialized with WriteBitAngle(16)
/// instead of raw 32-bit floats. Used by net_protocol 7 HL2/GMod 9 demos.
pub(crate) fn parse_usercmd_old(data: &[u8]) -> Option<UserCmd> {
    parse_usercmd_with_angles(data, UserCmdAngleMode::BitAngle16)
}

pub(crate) fn parse_usercmd_for_net(data: &[u8], net_protocol: i32) -> Option<UserCmd> {
    if net_protocol <= 7 {
        parse_usercmd_old(data)
    } else {
        parse_usercmd(data)
    }
}

pub(crate) fn parse_usercmd_for_demo(
    data: &[u8],
    net_protocol: i32,
    game_dir: &str,
) -> Option<UserCmd> {
    let mut ucmd = parse_usercmd_for_net(data, net_protocol)?;
    if net_protocol <= 7 && is_legacy_gmod_game(game_dir) {
        apply_legacy_gmod_usercmd_overrides(data, &mut ucmd);
    }
    Some(ucmd)
}

fn is_legacy_gmod_game(game_dir: &str) -> bool {
    game_dir.to_ascii_lowercase().contains("garrysmod")
}

fn apply_legacy_gmod_usercmd_overrides(data: &[u8], ucmd: &mut UserCmd) {
    if ucmd
        .buttons
        .is_some_and(|buttons| !plausible_source_buttons(buttons))
    {
        ucmd.buttons = None;
    }
    if ucmd
        .forwardmove
        .is_some_and(|value| !plausible_legacy_gmod_move(value))
    {
        ucmd.forwardmove = None;
    }
    if ucmd
        .sidemove
        .is_some_and(|value| !plausible_legacy_gmod_move(value))
    {
        ucmd.sidemove = None;
    }
    if ucmd
        .upmove
        .is_some_and(|value| !plausible_legacy_gmod_move(value))
    {
        ucmd.upmove = None;
    }

    if data.len() >= 20 {
        let buttons = u32::from_le_bytes(data[16..20].try_into().unwrap());
        if plausible_source_buttons(buttons) {
            ucmd.buttons = Some(buttons);
        }
    }

    if let Some(buttons) = ucmd.buttons {
        let fwd = button_axis(buttons, IN_FORWARD, IN_BACK);
        let side = button_axis(buttons, IN_MOVERIGHT, IN_MOVELEFT);
        ucmd.forwardmove = Some(fwd);
        ucmd.sidemove = Some(side);
        ucmd.upmove = Some(0.0);
    }
}

fn plausible_source_buttons(buttons: u32) -> bool {
    buttons & !SOURCE_BUTTON_MASK == 0
}

fn plausible_legacy_gmod_move(value: f32) -> bool {
    value.is_finite() && value.abs() <= 1000.0
}

fn button_axis(buttons: u32, positive: u32, negative: u32) -> f32 {
    let mut out = 0.0;
    if buttons & positive != 0 {
        out += 400.0;
    }
    if buttons & negative != 0 {
        out -= 400.0;
    }
    out
}

fn parse_usercmd_with_angles(data: &[u8], angle_mode: UserCmdAngleMode) -> Option<UserCmd> {
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

    if has!() {
        ucmd.command_number = br.read_u32();
    }
    if has!() {
        ucmd.tick_count = br.read_u32();
    }
    if has!() {
        ucmd.pitch = read_usercmd_angle(&mut br, &angle_mode);
    }
    if has!() {
        ucmd.yaw = read_usercmd_angle(&mut br, &angle_mode);
    }
    if has!() {
        ucmd.roll = read_usercmd_angle(&mut br, &angle_mode);
    }
    if has!() {
        ucmd.forwardmove = read_usercmd_move(&mut br, &angle_mode);
    }
    if has!() {
        ucmd.sidemove = read_usercmd_move(&mut br, &angle_mode);
    }
    if has!() {
        ucmd.upmove = read_usercmd_move(&mut br, &angle_mode);
    }
    if has!() {
        ucmd.buttons = br.read_u32();
    }
    if has!() {
        ucmd.impulse = br.read_byte();
    }
    if has!() {
        ucmd.weaponselect = br.read_bits(MAX_EDICT_BITS);
        if has!() {
            ucmd.weaponsubtype = br.read_bits(WEAPON_SUBTYPE_BITS);
        }
    }
    if has!() {
        ucmd.mousedx = br.read_i16();
    }
    if has!() {
        ucmd.mousedy = br.read_i16();
    }

    Some(ucmd)
}

fn read_usercmd_angle(br: &mut BitReader, mode: &UserCmdAngleMode) -> Option<f32> {
    match mode {
        UserCmdAngleMode::Float32 => br.read_bit_float(),
        UserCmdAngleMode::BitAngle16 => {
            let raw = br.read_bits(16)? as f32;
            let angle = raw * (360.0 / 65536.0);
            Some(if angle >= 180.0 { angle - 360.0 } else { angle })
        }
    }
}

fn read_usercmd_move(br: &mut BitReader, mode: &UserCmdAngleMode) -> Option<f32> {
    match mode {
        UserCmdAngleMode::Float32 => br.read_bit_float(),
        UserCmdAngleMode::BitAngle16 => br.read_i16().map(|v| v as f32),
    }
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
        (IN_CANCEL, "CANCEL"),
        (IN_RUN, "RUN"),
        (IN_RELOAD, "RELOAD"),
        (IN_ALT1, "ALT1"),
        (IN_ALT2, "ALT2"),
        (IN_SCORE, "SCORE"),
        (IN_SPEED, "SPEED"),
        (IN_WALK, "WALK"),
        (IN_ZOOM, "ZOOM"),
        (IN_WEAPON1, "WEAPON1"),
        (IN_WEAPON2, "WEAPON2"),
        (IN_BULLRUSH, "BULLRUSH"),
        (IN_GRENADE1, "GRENADE1"),
        (IN_GRENADE2, "GRENADE2"),
        (IN_ATTACK3, "ATTACK3"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn push_bits(out: &mut Vec<u8>, bit_pos: &mut usize, value: u32, bits: u32) {
        for i in 0..bits {
            if *bit_pos / 8 >= out.len() {
                out.push(0);
            }
            if (value >> i) & 1 != 0 {
                out[*bit_pos / 8] |= 1 << (*bit_pos % 8);
            }
            *bit_pos += 1;
        }
    }

    #[test]
    fn parses_old_source_usercmd_bit_angles() {
        let mut bytes = Vec::new();
        let mut bit = 0usize;
        push_bits(&mut bytes, &mut bit, 1, 1);
        push_bits(&mut bytes, &mut bit, 7, 32);
        push_bits(&mut bytes, &mut bit, 1, 1);
        push_bits(&mut bytes, &mut bit, 11, 32);
        push_bits(&mut bytes, &mut bit, 1, 1);
        push_bits(&mut bytes, &mut bit, 0xFC00, 16); // -5.625 degrees
        push_bits(&mut bytes, &mut bit, 1, 1);
        push_bits(&mut bytes, &mut bit, 0x4000, 16); // 90 degrees
        push_bits(&mut bytes, &mut bit, 0, 1); // roll
        push_bits(&mut bytes, &mut bit, 1, 1);
        push_bits(&mut bytes, &mut bit, 400, 16);
        push_bits(&mut bytes, &mut bit, 0, 1); // sidemove
        push_bits(&mut bytes, &mut bit, 0, 1); // upmove
        push_bits(&mut bytes, &mut bit, 1, 1);
        push_bits(&mut bytes, &mut bit, 8, 32); // IN_FORWARD

        let cmd = parse_usercmd_old(&bytes).unwrap();
        assert_eq!(cmd.command_number, Some(7));
        assert_eq!(cmd.tick_count, Some(11));
        assert!((cmd.pitch.unwrap() + 5.625).abs() < 0.001);
        assert!((cmd.yaw.unwrap() - 90.0).abs() < 0.001);
        assert_eq!(cmd.forwardmove, Some(400.0));
        assert_eq!(cmd.buttons, Some(8));
    }

    #[test]
    fn overrides_legacy_gmod_buttons_from_extended_payload() {
        let mut bytes = Vec::new();
        let mut bit = 0usize;
        push_bits(&mut bytes, &mut bit, 1, 1);
        push_bits(&mut bytes, &mut bit, 7, 32);
        push_bits(&mut bytes, &mut bit, 0, 1); // tick
        push_bits(&mut bytes, &mut bit, 0, 1); // pitch
        push_bits(&mut bytes, &mut bit, 0, 1); // yaw
        push_bits(&mut bytes, &mut bit, 0, 1); // roll
        push_bits(&mut bytes, &mut bit, 0, 1); // forwardmove
        push_bits(&mut bytes, &mut bit, 0, 1); // sidemove
        push_bits(&mut bytes, &mut bit, 0, 1); // upmove
        push_bits(&mut bytes, &mut bit, 1, 1);
        push_bits(&mut bytes, &mut bit, IN_ATTACK2, 32);
        bytes.resize(20, 0);
        bytes[16..20].copy_from_slice(&(IN_ATTACK | IN_FORWARD | IN_MOVELEFT).to_le_bytes());

        let cmd = parse_usercmd_for_demo(&bytes, 7, "garrysmod10classic").unwrap();
        assert_eq!(cmd.buttons, Some(IN_ATTACK | IN_FORWARD | IN_MOVELEFT));
        assert_eq!(cmd.forwardmove, Some(400.0));
        assert_eq!(cmd.sidemove, Some(-400.0));
    }
}
