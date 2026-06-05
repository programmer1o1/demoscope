// ─── Source Engine CBitBuf reader (LSB-first within each byte) ───────────────
//
// The bit-packed net-message stream (usercmds, game events, string tables) is
// read through this. Reads past `max_bit` return None / false rather than
// panicking, so the parse loops bail cleanly on a malformed or truncated demo.

pub(crate) struct BitReader<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) bit_pos: usize,
    pub(crate) max_bit: usize, // hard upper limit; defaults to data.len()*8
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let max = data.len() * 8;
        BitReader { data, bit_pos: 0, max_bit: max }
    }

    pub(crate) fn new_at(data: &'a [u8], pos: usize) -> Self {
        let max = data.len() * 8;
        BitReader { data, bit_pos: pos, max_bit: max }
    }

    pub(crate) fn read_bits(&mut self, n: u32) -> Option<u32> {
        if self.max_bit < self.bit_pos + n as usize {
            return None;
        }
        let mut result = 0u32;
        for i in 0..n {
            let byte_idx = self.bit_pos / 8;
            let bit_idx = self.bit_pos % 8;
            let bit = ((self.data[byte_idx] >> bit_idx) & 1) as u32;
            result |= bit << i;
            self.bit_pos += 1;
        }
        Some(result)
    }

    pub(crate) fn read_u32(&mut self) -> Option<u32> {
        self.read_bits(32)
    }

    // WriteBitFloat stores the raw IEEE-754 bits.
    pub(crate) fn read_bit_float(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.read_u32()?))
    }

    pub(crate) fn read_i16(&mut self) -> Option<i16> {
        Some(self.read_bits(16)? as i16)
    }

    pub(crate) fn read_byte(&mut self) -> Option<u8> {
        Some(self.read_bits(8)? as u8)
    }

    pub(crate) fn skip(&mut self, n: u32) -> bool {
        if self.max_bit < self.bit_pos + n as usize {
            return false;
        }
        self.bit_pos += n as usize;
        true
    }

    pub(crate) fn bits_remaining(&self) -> usize {
        let total = self.data.len() * 8;
        if total > self.bit_pos { total - self.bit_pos } else { 0 }
    }

    pub(crate) fn try_read_cstring(&mut self, max: usize) -> Option<String> {
        let mut chars = Vec::new();
        for _ in 0..max {
            if self.bits_remaining() < 8 {
                return None;
            }
            let b = self.read_bits(8)? as u8;
            if b == 0 {
                break;
            }
            // Must be ASCII alphanumeric or underscore
            if !b.is_ascii_alphanumeric() && b != b'_' {
                return None;
            }
            chars.push(b);
        }
        Some(String::from_utf8(chars).ok()?)
    }

    pub(crate) fn read_cstring_any(&mut self, max: usize) -> Option<String> {
        let mut chars = Vec::new();
        for _ in 0..max {
            if self.bits_remaining() < 8 {
                return None;
            }
            let b = self.read_bits(8)? as u8;
            if b == 0 {
                break;
            }
            chars.push(b);
        }
        Some(String::from_utf8_lossy(&chars).into_owned())
    }
}
