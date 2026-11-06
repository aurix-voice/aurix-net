//! MSB-first bit packing.

#[derive(Debug, Default, Clone)]
pub struct BitWriter {
    buf: Vec<u8>,
    bit: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write_bits(&mut self, value: u64, nbits: u32) {
        debug_assert!(nbits <= 64);
        for i in (0..nbits).rev() {
            let b = (value >> i) & 1;
            if self.bit.is_multiple_of(8) {
                self.buf.push(0);
            }
            if b == 1 {
                let idx = self.bit / 8;
                self.buf[idx] |= 0x80 >> (self.bit % 8);
            }
            self.bit += 1;
        }
    }

    pub fn write_bool(&mut self, v: bool) {
        self.write_bits(v as u64, 1);
    }

    /// Unsigned integer in `[0, 2^nbits)`, clamped.
    pub fn write_uint(&mut self, v: u64, nbits: u32) {
        let max = if nbits >= 64 {
            u64::MAX
        } else {
            (1u64 << nbits) - 1
        };
        self.write_bits(v.min(max), nbits);
    }

    /// Signed integer via zigzag in `nbits`.
    pub fn write_int(&mut self, v: i64, nbits: u32) {
        let zz = ((v << 1) ^ (v >> 63)) as u64;
        self.write_uint(zz, nbits);
    }

    /// Quantizes `v` into `[min, max]` with step `res`.
    pub fn write_quantized(&mut self, v: f32, min: f32, max: f32, res: f32) {
        let (nbits, _) = quant_bits(min, max, res);
        let q = (((v.clamp(min, max) - min) / res) + 0.5) as u64;
        self.write_uint(q, nbits);
    }

    pub fn write_f32(&mut self, v: f32) {
        self.write_bits(v.to_bits() as u64, 32);
    }

    pub fn write_bytes(&mut self, b: &[u8]) {
        for x in b {
            self.write_bits(*x as u64, 8);
        }
    }

    pub fn bit_len(&self) -> usize {
        self.bit
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("read past end of buffer")]
pub struct Eof;

#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    buf: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, bit: 0 }
    }

    pub fn read_bits(&mut self, nbits: u32) -> Result<u64, Eof> {
        if self.bit + nbits as usize > self.buf.len() * 8 {
            return Err(Eof);
        }
        let mut v = 0u64;
        for _ in 0..nbits {
            let byte = self.buf[self.bit / 8];
            let b = (byte >> (7 - (self.bit % 8))) & 1;
            v = (v << 1) | b as u64;
            self.bit += 1;
        }
        Ok(v)
    }

    pub fn read_bool(&mut self) -> Result<bool, Eof> {
        Ok(self.read_bits(1)? == 1)
    }

    pub fn read_uint(&mut self, nbits: u32) -> Result<u64, Eof> {
        self.read_bits(nbits)
    }

    pub fn read_int(&mut self, nbits: u32) -> Result<i64, Eof> {
        let zz = self.read_bits(nbits)?;
        Ok(((zz >> 1) as i64) ^ -((zz & 1) as i64))
    }

    pub fn read_quantized(&mut self, min: f32, max: f32, res: f32) -> Result<f32, Eof> {
        let (nbits, _) = quant_bits(min, max, res);
        let q = self.read_uint(nbits)?;
        Ok((min + q as f32 * res).min(max))
    }

    pub fn read_f32(&mut self) -> Result<f32, Eof> {
        Ok(f32::from_bits(self.read_bits(32)? as u32))
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<Vec<u8>, Eof> {
        (0..n).map(|_| self.read_bits(8).map(|b| b as u8)).collect()
    }

    pub fn bits_left(&self) -> usize {
        self.buf.len() * 8 - self.bit
    }
}

/// Bits needed to encode `(max-min)/res` steps, and the step count.
pub fn quant_bits(min: f32, max: f32, res: f32) -> (u32, u64) {
    let steps = (((max - min) / res).ceil() as u64).max(1);
    let bits = 64 - steps.leading_zeros();
    (bits.max(1), steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_mixed() {
        let mut w = BitWriter::new();
        w.write_bool(true);
        w.write_uint(5, 3);
        w.write_int(-1234, 20);
        w.write_quantized(12.345, -100.0, 100.0, 0.01);
        w.write_f32(1.5);
        w.write_bytes(b"ab");
        let bits = w.bit_len();
        let buf = w.finish();
        assert_eq!(buf.len(), bits.div_ceil(8));
        let mut r = BitReader::new(&buf);
        assert!(r.read_bool().unwrap());
        assert_eq!(r.read_uint(3).unwrap(), 5);
        assert_eq!(r.read_int(20).unwrap(), -1234);
        assert!((r.read_quantized(-100.0, 100.0, 0.01).unwrap() - 12.345).abs() < 0.006);
        assert_eq!(r.read_f32().unwrap(), 1.5);
        assert_eq!(r.read_bytes(2).unwrap(), b"ab");
        // Only padding bits remain in the last byte.
        let pad = buf.len() * 8 - bits;
        assert_eq!(r.read_bits(pad as u32).unwrap(), 0);
        assert_eq!(r.read_bits(1), Err(Eof));
    }

    #[test]
    fn quant_bits_sizes() {
        assert_eq!(quant_bits(0.0, 1.0, 1.0).0, 1);
        assert_eq!(quant_bits(-1000.0, 1000.0, 0.01).0, 18);
    }
}
