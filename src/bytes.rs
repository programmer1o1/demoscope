// Little-endian byte readers over a `&[u8]`, plus a fixed-width C-string
// reader. These are the raw accessors the header/BSP/packet walkers lean on;
// the bit-level reads live in `bitreader`.

pub(crate) fn le_i32(data: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

pub(crate) fn le_f32(data: &[u8], off: usize) -> f32 {
    f32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

pub(crate) fn le_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(data[off..off + 2].try_into().unwrap())
}

pub(crate) fn le_i16_bytes(data: &[u8], off: usize) -> i16 {
    i16::from_le_bytes(data[off..off + 2].try_into().unwrap())
}

pub(crate) fn read_cstring(data: &[u8], off: usize, max: usize) -> String {
    let end = data[off..off + max]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(max);
    String::from_utf8_lossy(&data[off..off + end]).into_owned()
}
