// LSB-first bit reader for the Source 2 entity bitstream.
//
// Ported from dotabuff/manta `reader.go`. Source 2 packs entity data as a
// little-endian bit stream with several bespoke variable-length encodings
// (ubitvar, fieldpath ubitvar, coord/normal/angle floats). Every primitive here
// must be bit-exact or the whole tick desyncs.

pub struct BitReader<'a> {
    buf: &'a [u8],
    pos: usize,
    bit_val: u64,
    bit_count: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        BitReader { buf, pos: 0, bit_val: 0, bit_count: 0 }
    }

    /// Unread whole bytes remaining (ignores the partial bit cache).
    pub fn rem_bytes(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn next_byte(&mut self) -> u8 {
        let b = self.buf.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    /// Read `n` (0..=32) bits as a u32.
    pub fn read_bits(&mut self, n: u32) -> u32 {
        while n > self.bit_count {
            self.bit_val |= (self.next_byte() as u64) << self.bit_count;
            self.bit_count += 8;
        }
        let x = self.bit_val & (((1u64) << n) - 1);
        self.bit_val >>= n;
        self.bit_count -= n;
        x as u32
    }

    pub fn read_bit(&mut self) -> bool {
        self.read_bits(1) == 1
    }

    fn read_byte(&mut self) -> u8 {
        if self.bit_count == 0 {
            self.next_byte()
        } else {
            self.read_bits(8) as u8
        }
    }

    pub fn read_bytes(&mut self, n: usize) -> Vec<u8> {
        if self.bit_count == 0 {
            let end = (self.pos + n).min(self.buf.len());
            let out = self.buf[self.pos..end].to_vec();
            self.pos += n;
            out
        } else {
            (0..n).map(|_| self.read_byte()).collect()
        }
    }

    /// Read `n` bits, packed into bytes (last group is the low bits).
    pub fn read_bits_as_bytes(&mut self, mut n: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((n as usize + 7) / 8);
        while n >= 8 {
            out.push(self.read_byte());
            n -= 8;
        }
        if n > 0 {
            out.push(self.read_bits(n) as u8);
        }
        out
    }

    pub fn read_var_u32(&mut self) -> u32 {
        let mut x: u32 = 0;
        let mut s: u32 = 0;
        loop {
            let b = self.read_byte() as u32;
            x |= (b & 0x7f) << s;
            s += 7;
            if (b & 0x80) == 0 || s == 35 {
                break;
            }
        }
        x
    }

    pub fn read_var_i32(&mut self) -> i32 {
        let ux = self.read_var_u32();
        let x = (ux >> 1) as i32;
        if ux & 1 != 0 { !x } else { x }
    }

    pub fn read_var_u64(&mut self) -> u64 {
        let mut x: u64 = 0;
        let mut s: u32 = 0;
        let mut i = 0;
        loop {
            let b = self.read_byte();
            if b < 0x80 {
                return x | ((b as u64) << s);
            }
            x |= ((b & 0x7f) as u64) << s;
            s += 7;
            i += 1;
            if i > 9 {
                return x;
            }
        }
    }

    pub fn read_le_u64(&mut self) -> u64 {
        let b = self.read_bytes(8);
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&b[..8.min(b.len())]);
        u64::from_le_bytes(arr)
    }

    /// Variable-length uint: 6-bit group whose top 2 bits select a tail width.
    pub fn read_ubit_var(&mut self) -> u32 {
        let ret = self.read_bits(6);
        match ret & 0x30 {
            16 => (ret & 15) | (self.read_bits(4) << 4),
            32 => (ret & 15) | (self.read_bits(8) << 4),
            48 => (ret & 15) | (self.read_bits(28) << 4),
            _ => ret,
        }
    }

    /// Field-path variable uint (escalating 2/4/10/17/31-bit windows).
    pub fn read_ubit_var_fp(&mut self) -> u32 {
        if self.read_bit() { return self.read_bits(2); }
        if self.read_bit() { return self.read_bits(4); }
        if self.read_bit() { return self.read_bits(10); }
        if self.read_bit() { return self.read_bits(17); }
        self.read_bits(31)
    }

    pub fn read_string(&mut self) -> String {
        let mut out = Vec::new();
        loop {
            if self.rem_bytes() == 0 && self.bit_count == 0 {
                break;
            }
            let b = self.read_byte();
            if b == 0 {
                break;
            }
            out.push(b);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    pub fn read_float_noscale(&mut self) -> f32 {
        f32::from_bits(self.read_bits(32))
    }

    pub fn read_coord(&mut self) -> f32 {
        let mut value = 0.0f32;
        let mut intval = self.read_bits(1);
        let mut fractval = self.read_bits(1);
        if intval != 0 || fractval != 0 {
            let signbit = self.read_bit();
            if intval != 0 {
                intval = self.read_bits(14) + 1;
            }
            if fractval != 0 {
                fractval = self.read_bits(5);
            }
            value = intval as f32 + fractval as f32 * (1.0 / (1 << 5) as f32);
            if signbit {
                value = -value;
            }
        }
        value
    }

    pub fn read_angle(&mut self, n: u32) -> f32 {
        self.read_bits(n) as f32 * 360.0 / (1u32 << n) as f32
    }

    pub fn read_normal(&mut self) -> f32 {
        let is_neg = self.read_bit();
        let len = self.read_bits(11);
        let ret = len as f32 * (1.0 / ((1 << 11) as f32 - 1.0));
        if is_neg { -ret } else { ret }
    }

    pub fn read_3bit_normal(&mut self) -> [f32; 3] {
        let mut ret = [0.0f32; 3];
        let has_x = self.read_bit();
        let has_y = self.read_bit();
        if has_x { ret[0] = self.read_normal(); }
        if has_y { ret[1] = self.read_normal(); }
        let neg_z = self.read_bit();
        let prodsum = ret[0] * ret[0] + ret[1] * ret[1];
        if prodsum < 1.0 {
            ret[2] = (1.0 - prodsum).sqrt();
        }
        if neg_z { ret[2] = -ret[2]; }
        ret
    }
}
