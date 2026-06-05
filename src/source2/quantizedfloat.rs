// Quantized-float decoder. Direct port of dotabuff/manta `quantizedfloat.go`.
//
// A networked float compressed into `bit_count` bits over [low, high], with
// flags for round-up/down, encode-zero and integer encoding. The constructor
// pre-computes the multipliers; `decode` reads the bits and reconstructs.

use super::bitreader::BitReader;

const QFF_ROUNDDOWN: u32 = 1 << 0;
const QFF_ROUNDUP: u32 = 1 << 1;
const QFF_ENCODE_ZERO: u32 = 1 << 2;
const QFF_ENCODE_INTEGERS: u32 = 1 << 3;

#[derive(Clone)]
pub struct QuantizedFloatDecoder {
    low: f32,
    high: f32,
    high_low_mul: f32,
    dec_mul: f32,
    bit_count: u32,
    flags: u32,
    no_scale: bool,
}

impl QuantizedFloatDecoder {
    pub fn new(bit_count: Option<i32>, flags: Option<i32>, low: Option<f32>, high: Option<f32>) -> Self {
        let mut q = QuantizedFloatDecoder {
            low: 0.0,
            high: 1.0,
            high_low_mul: 0.0,
            dec_mul: 0.0,
            bit_count: 0,
            flags: 0,
            no_scale: false,
        };

        let bc = bit_count.unwrap_or(0);
        if bc == 0 || bc >= 32 {
            q.no_scale = true;
            q.bit_count = 32;
            return q;
        }
        q.no_scale = false;
        q.bit_count = bc as u32;
        q.low = low.unwrap_or(0.0);
        q.high = high.unwrap_or(1.0);
        q.flags = flags.map(|f| f as u32).unwrap_or(0);

        q.validate_flags();

        let mut steps: u32 = 1u32 << q.bit_count;

        if q.flags & QFF_ROUNDDOWN != 0 {
            let range = q.high - q.low;
            let offset = range / steps as f32;
            q.high -= offset;
        } else if q.flags & QFF_ROUNDUP != 0 {
            let range = q.high - q.low;
            let offset = range / steps as f32;
            q.low += offset;
        }

        if q.flags & QFF_ENCODE_INTEGERS != 0 {
            let mut delta = q.high - q.low;
            if delta < 1.0 {
                delta = 1.0;
            }
            let delta_log2 = (delta as f64).log2().ceil();
            let range2 = 1u32 << (delta_log2 as u32);
            let mut bc2 = q.bit_count;
            loop {
                if (1u32 << bc2) > range2 {
                    break;
                } else {
                    bc2 += 1;
                }
            }
            if bc2 > q.bit_count {
                q.bit_count = bc2;
                steps = 1u32 << q.bit_count;
            }
            let offset = range2 as f32 / steps as f32;
            q.high = q.low + range2 as f32 - offset;
        }

        q.assign_multipliers(steps);

        // Remove unnecessary flags now that the lattice is fixed.
        if q.flags & QFF_ROUNDDOWN != 0 && q.quantize(q.low) == q.low {
            q.flags &= !QFF_ROUNDDOWN;
        }
        if q.flags & QFF_ROUNDUP != 0 && q.quantize(q.high) == q.high {
            q.flags &= !QFF_ROUNDUP;
        }
        if q.flags & QFF_ENCODE_ZERO != 0 && q.quantize(0.0) == 0.0 {
            q.flags &= !QFF_ENCODE_ZERO;
        }

        q
    }

    fn validate_flags(&mut self) {
        if self.flags == 0 {
            return;
        }
        if (self.low == 0.0 && self.flags & QFF_ROUNDDOWN != 0)
            || (self.high == 0.0 && self.flags & QFF_ROUNDUP != 0)
        {
            self.flags &= !QFF_ENCODE_ZERO;
        }
        if self.low == 0.0 && self.flags & QFF_ENCODE_ZERO != 0 {
            self.flags |= QFF_ROUNDDOWN;
            self.flags &= !QFF_ENCODE_ZERO;
        }
        if self.high == 0.0 && self.flags & QFF_ENCODE_ZERO != 0 {
            self.flags |= QFF_ROUNDUP;
            self.flags &= !QFF_ENCODE_ZERO;
        }
        if self.low > 0.0 || self.high < 0.0 {
            self.flags &= !QFF_ENCODE_ZERO;
        }
        if self.flags & QFF_ENCODE_INTEGERS != 0 {
            self.flags &= !(QFF_ROUNDUP | QFF_ROUNDDOWN | QFF_ENCODE_ZERO);
        }
    }

    fn assign_multipliers(&mut self, steps: u32) {
        self.high_low_mul = 0.0;
        let range = self.high - self.low;
        let high: u32 = if self.bit_count == 32 { 0xFFFF_FFFE } else { (1u32 << self.bit_count) - 1 };

        let mut high_mul: f32 = if range.abs() <= 0.0 {
            high as f32
        } else {
            high as f32 / range
        };

        if high_mul * range > high as f32 || (high_mul as f64 * range as f64) > high as f64 {
            for &mult in &[0.9999f32, 0.99, 0.9, 0.8, 0.7] {
                high_mul = high as f32 / range * mult;
                if high_mul * range > high as f32 || (high_mul as f64 * range as f64) > high as f64 {
                    continue;
                }
                break;
            }
        }

        self.high_low_mul = high_mul;
        self.dec_mul = 1.0 / (steps - 1) as f32;
    }

    fn quantize(&self, val: f32) -> f32 {
        if val < self.low {
            return self.low;
        } else if val > self.high {
            return self.high;
        }
        let i = ((val - self.low) * self.high_low_mul) as u32;
        self.low + (self.high - self.low) * (i as f32 * self.dec_mul)
    }

    pub fn decode(&self, r: &mut BitReader) -> f32 {
        if self.no_scale {
            return r.read_float_noscale();
        }
        if self.flags & QFF_ROUNDDOWN != 0 && r.read_bit() {
            return self.low;
        }
        if self.flags & QFF_ROUNDUP != 0 && r.read_bit() {
            return self.high;
        }
        if self.flags & QFF_ENCODE_ZERO != 0 && r.read_bit() {
            return 0.0;
        }
        self.low + (self.high - self.low) * r.read_bits(self.bit_count) as f32 * self.dec_mul
    }
}
