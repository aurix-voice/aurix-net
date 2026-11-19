//! MSB-first bit packing.

#[derive(Debug, Default, Clone)]
pub struct BitWriter {
    buf: Vec<u8>,
    /// Pending bits, MSB-aligned in the low `nscratch` bits.
    scratch: u64,
    nscratch: u32,
    bit: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn write_bits(&mut self, value: u64, nbits: u32) {
        debug_assert!(nbits <= 64);
        if nbits == 0 {
            return;
        }
        let value = if nbits == 64 {
            value
        } else {
            value & ((1u64 << nbits) - 1)
        };
        self.bit += nbits as usize;
        // Push in two halves when the scratch would overflow 64 bits.
        let mut remaining = nbits;
        let mut rest = value;
        while remaining > 0 {
            let take = remaining.min(64 - self.nscratch);
            let part = rest >> (remaining - take);
            let part = if take == 64 {
                part
            } else {
                part & ((1u64 << take) - 1)
            };
            self.scratch = if take == 64 {
                part
            } else {
                (self.scratch << take) | part
            };
            self.nscratch += take;
            remaining -= take;
            if remaining > 0 {
                rest &= (1u64 << remaining) - 1;
            }
            while self.nscratch >= 8 {
                let byte = (self.scratch >> (self.nscratch - 8)) as u8;
                self.buf.push(byte);
                self.nscratch -= 8;
                if self.nscratch < 64 {
                    self.scratch &= (1u64 << self.nscratch) - 1;
                }
            }
        }
    }

    pub fn write_bool(&mut self, value: bool) {
        self.write_bits(value as u64, 1);
    }

    /// Unsigned integer in `[0, 2^nbits)`, clamped.
    pub fn write_uint(&mut self, value: u64, nbits: u32) {
        let max = if nbits >= 64 {
            u64::MAX
        } else {
            (1u64 << nbits) - 1
        };
        self.write_bits(value.min(max), nbits);
    }

    /// Signed integer via zigzag in `nbits`.
    pub fn write_int(&mut self, value: i64, nbits: u32) {
        let zz = ((value << 1) ^ (value >> 63)) as u64;
        self.write_uint(zz, nbits);
    }

    /// Quantizes `value` into `[min, max]` with step `res`.
    pub fn write_quantized(&mut self, value: f32, min: f32, max: f32, res: f32) {
        let (nbits, _) = quant_bits(min, max, res);
        let steps = (((value.clamp(min, max) - min) / res) + 0.5) as u64;
        self.write_uint(steps, nbits);
    }

    pub fn write_f32(&mut self, value: f32) {
        self.write_bits(value.to_bits() as u64, 32);
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        if self.nscratch == 0 {
            self.buf.extend_from_slice(bytes);
            self.bit += bytes.len() * 8;
            return;
        }
        for byte in bytes {
            self.write_bits(*byte as u64, 8);
        }
    }

    pub fn bit_len(&self) -> usize {
        self.bit
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.nscratch > 0 {
            let byte = (self.scratch << (8 - self.nscratch)) as u8;
            self.buf.push(byte);
            self.nscratch = 0;
        }
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

    #[inline]
    pub fn read_bits(&mut self, nbits: u32) -> Result<u64, Eof> {
        if nbits == 0 {
            return Ok(0);
        }
        if self.bit + nbits as usize > self.buf.len() * 8 {
            return Err(Eof);
        }
        let mut value = 0u64;
        let mut remaining = nbits;
        while remaining > 0 {
            let byte = self.buf[self.bit / 8];
            let off = (self.bit % 8) as u32;
            let avail = 8 - off;
            let take = remaining.min(avail);
            let chunk = ((byte as u32) >> (avail - take)) & ((1u32 << take) - 1);
            value = (value << take) | chunk as u64;
            self.bit += take as usize;
            remaining -= take;
        }
        Ok(value)
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
        let steps = self.read_uint(nbits)?;
        Ok((min + steps as f32 * res).min(max))
    }

    pub fn read_f32(&mut self) -> Result<f32, Eof> {
        Ok(f32::from_bits(self.read_bits(32)? as u32))
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<Vec<u8>, Eof> {
        if self.bit.is_multiple_of(8) {
            let start = self.bit / 8;
            if start + len > self.buf.len() {
                return Err(Eof);
            }
            self.bit += len * 8;
            return Ok(self.buf[start..start + len].to_vec());
        }
        (0..len)
            .map(|_| self.read_bits(8).map(|b| b as u8))
            .collect()
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
        let mut writer = BitWriter::new();
        writer.write_bool(true);
        writer.write_uint(5, 3);
        writer.write_int(-1234, 20);
        writer.write_quantized(12.345, -100.0, 100.0, 0.01);
        writer.write_f32(1.5);
        writer.write_bytes(b"ab");
        let bits = writer.bit_len();
        let buf = writer.finish();
        assert_eq!(buf.len(), bits.div_ceil(8));
        let mut reader = BitReader::new(&buf);
        assert!(reader.read_bool().unwrap());
        assert_eq!(reader.read_uint(3).unwrap(), 5);
        assert_eq!(reader.read_int(20).unwrap(), -1234);
        assert!((reader.read_quantized(-100.0, 100.0, 0.01).unwrap() - 12.345).abs() < 0.006);
        assert_eq!(reader.read_f32().unwrap(), 1.5);
        assert_eq!(reader.read_bytes(2).unwrap(), b"ab");
        // Only padding bits remain in the last byte.
        let pad = buf.len() * 8 - bits;
        assert_eq!(reader.read_bits(pad as u32).unwrap(), 0);
        assert_eq!(reader.read_bits(1), Err(Eof));
    }

    #[test]
    fn word_boundaries() {
        let mut writer = BitWriter::new();
        writer.write_bits(0x1, 3);
        writer.write_bits(u64::MAX, 64);
        writer.write_bits(0xABCDEF, 24);
        writer.write_bits(0, 61);
        writer.write_bits(0x5, 3);
        let buf = writer.finish();
        let mut reader = BitReader::new(&buf);
        assert_eq!(reader.read_bits(3).unwrap(), 1);
        assert_eq!(reader.read_bits(64).unwrap(), u64::MAX);
        assert_eq!(reader.read_bits(24).unwrap(), 0xABCDEF);
        assert_eq!(reader.read_bits(61).unwrap(), 0);
        assert_eq!(reader.read_bits(3).unwrap(), 5);
    }

    #[test]
    fn quant_bits_sizes() {
        assert_eq!(quant_bits(0.0, 1.0, 1.0).0, 1);
        assert_eq!(quant_bits(-1000.0, 1000.0, 0.01).0, 18);
    }
}
