// Bit-level reader for the Source Engine demo format.
//
// Source uses LSB-first bit ordering within each byte: the first bit read
// is the low bit of byte 0, then bit 1 of byte 0, …, bit 7 of byte 0,
// then low bit of byte 1, and so on. Values that span byte boundaries are
// reassembled in that order.
//
// This module is the single primitive that every Source-format decoder
// (DataTables, PacketEntities, SendProps, StringTables) builds on.

pub struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
    max_bit: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let max = data.len() * 8;
        BitReader { data, bit_pos: 0, max_bit: max }
    }

    pub fn new_at(data: &'a [u8], pos: usize) -> Self {
        let max = data.len() * 8;
        BitReader { data, bit_pos: pos, max_bit: max }
    }

    /// Lower the upper bound on this reader. Used to constrain a sub-region
    /// (e.g. an entity-updates section) so reads past its end fail cleanly.
    pub fn set_max_bit(&mut self, max: usize) {
        self.max_bit = self.max_bit.min(max);
    }

    pub fn bit_pos(&self) -> usize { self.bit_pos }
    pub fn set_bit_pos(&mut self, p: usize) { self.bit_pos = p; }
    pub fn bits_remaining(&self) -> usize {
        self.max_bit.saturating_sub(self.bit_pos)
    }
    pub fn max_bit(&self) -> usize { self.max_bit }

    pub fn read_bits(&mut self, n: u32) -> Option<u32> {
        if n == 0 { return Some(0); }
        if self.max_bit < self.bit_pos + n as usize { return None; }
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

    pub fn read_u32(&mut self) -> Option<u32> { self.read_bits(32) }
    pub fn read_byte(&mut self) -> Option<u8> { Some(self.read_bits(8)? as u8) }
    pub fn read_i16(&mut self) -> Option<i16> { Some(self.read_bits(16)? as i16) }

    /// Read 1 bit as a boolean
    pub fn read_bool(&mut self) -> Option<bool> { Some(self.read_bits(1)? != 0) }

    /// Read a signed n-bit integer (sign-extends the high bit)
    pub fn read_signed(&mut self, n: u32) -> Option<i32> {
        let raw = self.read_bits(n)?;
        // Sign-extend
        if n < 32 && raw & (1 << (n - 1)) != 0 {
            Some((raw | (!0u32 << n)) as i32)
        } else {
            Some(raw as i32)
        }
    }

    /// IEEE-754 float (raw 32 bits)
    pub fn read_bit_float(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.read_u32()?))
    }

    /// Skip n bits forward. Returns false if there aren't enough bits.
    pub fn skip(&mut self, n: u32) -> bool {
        if self.max_bit < self.bit_pos + n as usize { return false; }
        self.bit_pos += n as usize;
        true
    }

    /// Read a null-terminated string up to `max` bytes.
    pub fn read_cstring(&mut self, max: usize) -> Option<String> {
        let mut chars = Vec::with_capacity(32);
        for _ in 0..max {
            if self.bits_remaining() < 8 { return None; }
            let b = self.read_bits(8)? as u8;
            if b == 0 { break; }
            chars.push(b);
        }
        Some(String::from_utf8_lossy(&chars).into_owned())
    }

    /// Read a Source "var int" - 7 bits per byte, low bit is continue flag.
    /// Used for some message sizes in PacketEntities.
    pub fn read_var_u32(&mut self) -> Option<u32> {
        let mut result = 0u32;
        for shift in (0..32).step_by(7) {
            let b = self.read_byte()? as u32;
            result |= (b & 0x7F) << shift;
            if b & 0x80 == 0 { break; }
        }
        Some(result)
    }

    /// Read a UBitInt - Source's variable-length unsigned integer used in
    /// PacketEntities. The leading 2 bits encode how many extra bits follow:
    ///   00 → value is 0..15  (4 bits total)
    ///   01 → value is 16..31, read 4 more bits then add 16
    ///   10 → read 8 more bits then add 256
    ///   11 → read 28 more bits
    /// Reference: Source SDK `ReadUBitVar`.
    pub fn read_ubit_var(&mut self) -> Option<u32> {
        let v = self.read_bits(6)?;
        match v & 0x30 {
            0x10 => Some((v & 0x0F) | (self.read_bits(4)? << 4)),
            0x20 => Some((v & 0x0F) | (self.read_bits(8)? << 4)),
            0x30 => Some((v & 0x0F) | (self.read_bits(28)? << 4)),
            _    => Some(v & 0x0F),
        }
    }

    /// Read a UBitInt encoded for "field path" / entity-index style use.
    /// First 4 bits give a value 0..15; if the high bit is set in a different
    /// way, more bits follow. This is the entity-handle / prop-index format
    /// used inside svc_PacketEntities.
    pub fn read_ubit_int(&mut self) -> Option<u32> {
        let mut ret = self.read_bits(4)?;
        match self.read_bits(2)? {
            0 => { /* nothing */ }
            1 => ret |= self.read_bits(4)? << 4,
            2 => ret |= self.read_bits(8)? << 4,
            3 => ret |= self.read_bits(28)? << 4,
            _ => unreachable!(),
        }
        Some(ret)
    }
}
