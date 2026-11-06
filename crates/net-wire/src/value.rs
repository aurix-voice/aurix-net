//! Dynamic component values plus full and delta encoding against the schema.

use crate::bits::{BitReader, BitWriter, Eof};
use crate::schema::{Component, FieldType};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Bool(bool),
    UInt(u64),
    Int(i64),
    F32(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    /// x, y, z, w
    Quat([f32; 4]),
    String(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentValue {
    pub fields: Vec<Value>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("value does not match field type at index {0}")]
    Type(usize),
    #[error("field count mismatch")]
    Arity,
    #[error(transparent)]
    Eof(#[from] Eof),
    #[error("invalid utf-8 in string field")]
    Utf8,
}

const QUAT_BITS: u32 = 10;

impl ComponentValue {
    /// Default (zero) value for a component.
    pub fn zero(c: &Component) -> Self {
        Self {
            fields: c
                .fields
                .iter()
                .map(|f| match &f.ty {
                    FieldType::Bool => Value::Bool(false),
                    FieldType::UInt { .. } => Value::UInt(0),
                    FieldType::Int { .. } => Value::Int(0),
                    FieldType::F32 | FieldType::Quant { .. } => Value::F32(0.0),
                    FieldType::Vec2 { .. } => Value::Vec2([0.0; 2]),
                    FieldType::Vec3 { .. } => Value::Vec3([0.0; 3]),
                    FieldType::Quat => Value::Quat([0.0, 0.0, 0.0, 1.0]),
                    FieldType::String { .. } => Value::String(String::new()),
                    FieldType::Bytes { .. } => Value::Bytes(Vec::new()),
                })
                .collect(),
        }
    }

    fn write_field(
        w: &mut BitWriter,
        ty: &FieldType,
        v: &Value,
        idx: usize,
    ) -> Result<(), CodecError> {
        match (ty, v) {
            (FieldType::Bool, Value::Bool(b)) => w.write_bool(*b),
            (FieldType::UInt { bits }, Value::UInt(x)) => w.write_uint(*x, *bits),
            (FieldType::Int { bits }, Value::Int(x)) => w.write_int(*x, *bits),
            (FieldType::F32, Value::F32(x)) => w.write_f32(*x),
            (FieldType::Quant { min, max, res }, Value::F32(x)) => {
                w.write_quantized(*x, *min, *max, *res)
            }
            (FieldType::Vec2 { min, max, res }, Value::Vec2(a)) => {
                for x in a {
                    w.write_quantized(*x, *min, *max, *res);
                }
            }
            (FieldType::Vec3 { min, max, res }, Value::Vec3(a)) => {
                for x in a {
                    w.write_quantized(*x, *min, *max, *res);
                }
            }
            (FieldType::Quat, Value::Quat(q)) => write_quat(w, *q),
            (FieldType::String { max }, Value::String(s)) => {
                let b = &s.as_bytes()[..s.len().min(*max as usize)];
                let b = truncate_utf8(b);
                w.write_uint(b.len() as u64, len_bits(*max));
                w.write_bytes(b);
            }
            (FieldType::Bytes { max }, Value::Bytes(b)) => {
                let b = &b[..b.len().min(*max as usize)];
                w.write_uint(b.len() as u64, len_bits(*max));
                w.write_bytes(b);
            }
            _ => return Err(CodecError::Type(idx)),
        }
        Ok(())
    }

    fn read_field(r: &mut BitReader, ty: &FieldType) -> Result<Value, CodecError> {
        Ok(match ty {
            FieldType::Bool => Value::Bool(r.read_bool()?),
            FieldType::UInt { bits } => Value::UInt(r.read_uint(*bits)?),
            FieldType::Int { bits } => Value::Int(r.read_int(*bits)?),
            FieldType::F32 => Value::F32(r.read_f32()?),
            FieldType::Quant { min, max, res } => Value::F32(r.read_quantized(*min, *max, *res)?),
            FieldType::Vec2 { min, max, res } => Value::Vec2([
                r.read_quantized(*min, *max, *res)?,
                r.read_quantized(*min, *max, *res)?,
            ]),
            FieldType::Vec3 { min, max, res } => Value::Vec3([
                r.read_quantized(*min, *max, *res)?,
                r.read_quantized(*min, *max, *res)?,
                r.read_quantized(*min, *max, *res)?,
            ]),
            FieldType::Quat => Value::Quat(read_quat(r)?),
            FieldType::String { max } => {
                let n = r.read_uint(len_bits(*max))? as usize;
                Value::String(String::from_utf8(r.read_bytes(n)?).map_err(|_| CodecError::Utf8)?)
            }
            FieldType::Bytes { max } => {
                let n = r.read_uint(len_bits(*max))? as usize;
                Value::Bytes(r.read_bytes(n)?)
            }
        })
    }

    /// All fields.
    pub fn encode_full(&self, c: &Component, w: &mut BitWriter) -> Result<(), CodecError> {
        if self.fields.len() != c.fields.len() {
            return Err(CodecError::Arity);
        }
        for (i, (f, v)) in c.fields.iter().zip(&self.fields).enumerate() {
            Self::write_field(w, &f.ty, v, i)?;
        }
        Ok(())
    }

    pub fn decode_full(c: &Component, r: &mut BitReader) -> Result<Self, CodecError> {
        Ok(Self {
            fields: c
                .fields
                .iter()
                .map(|f| Self::read_field(r, &f.ty))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Change mask (one bit per field) followed by changed fields only. Returns false when
    /// nothing changed (nothing written).
    pub fn encode_delta(
        &self,
        base: &Self,
        c: &Component,
        w: &mut BitWriter,
    ) -> Result<bool, CodecError> {
        if self.fields.len() != c.fields.len() || base.fields.len() != c.fields.len() {
            return Err(CodecError::Arity);
        }
        let changed: Vec<bool> = self
            .fields
            .iter()
            .zip(&base.fields)
            .map(|(a, b)| a != b)
            .collect();
        if !changed.iter().any(|c| *c) {
            return Ok(false);
        }
        for ch in &changed {
            w.write_bool(*ch);
        }
        for (i, (f, v)) in c.fields.iter().zip(&self.fields).enumerate() {
            if changed[i] {
                Self::write_field(w, &f.ty, v, i)?;
            }
        }
        Ok(true)
    }

    pub fn decode_delta(base: &Self, c: &Component, r: &mut BitReader) -> Result<Self, CodecError> {
        let mut changed = Vec::with_capacity(c.fields.len());
        for _ in 0..c.fields.len() {
            changed.push(r.read_bool()?);
        }
        let mut out = base.clone();
        for (i, f) in c.fields.iter().enumerate() {
            if changed[i] {
                out.fields[i] = Self::read_field(r, &f.ty)?;
            }
        }
        Ok(out)
    }
}

fn len_bits(max: u32) -> u32 {
    (64 - (max as u64).leading_zeros()).max(1)
}

fn truncate_utf8(b: &[u8]) -> &[u8] {
    let mut n = b.len();
    while n > 0 && std::str::from_utf8(&b[..n]).is_err() {
        n -= 1;
    }
    &b[..n]
}

/// Smallest-three: drop the largest-magnitude component, send the other three in [-1/√2, 1/√2].
fn write_quat(w: &mut BitWriter, q: [f32; 4]) {
    let len = (q.iter().map(|x| x * x).sum::<f32>()).sqrt();
    let q = if len > 0.0 {
        q.map(|x| x / len)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    let mut largest = 0;
    for (i, x) in q.iter().enumerate().skip(1) {
        if x.abs() > q[largest].abs() {
            largest = i;
        }
    }
    let sign = if q[largest] < 0.0 { -1.0 } else { 1.0 };
    w.write_uint(largest as u64, 2);
    let lim = std::f32::consts::FRAC_1_SQRT_2;
    for (i, x) in q.iter().enumerate() {
        if i != largest {
            w.write_quantized(
                x * sign,
                -lim,
                lim,
                2.0 * lim / ((1u32 << QUAT_BITS) - 1) as f32,
            );
        }
    }
}

fn read_quat(r: &mut BitReader) -> Result<[f32; 4], Eof> {
    let largest = r.read_uint(2)? as usize;
    let lim = std::f32::consts::FRAC_1_SQRT_2;
    let mut q = [0.0f32; 4];
    let mut sum = 0.0;
    for (i, slot) in q.iter_mut().enumerate() {
        if i != largest {
            let v = r.read_quantized(-lim, lim, 2.0 * lim / ((1u32 << QUAT_BITS) - 1) as f32)?;
            *slot = v;
            sum += v * v;
        }
    }
    q[largest] = (1.0 - sum).max(0.0).sqrt();
    Ok(q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Schema;

    fn schema() -> Schema {
        Schema::parse(
            "component T {\n pos: vec3 @range(-100, 100, 0.01)\n rot: quat\n hp: u8\n name: string(8)\n dir: i16\n raw: f32\n}",
        )
        .unwrap()
    }

    #[test]
    fn full_roundtrip_and_sizes() {
        let s = schema();
        let c = s.component("T").unwrap();
        let v = ComponentValue {
            fields: vec![
                Value::Vec3([1.23, -45.67, 99.99]),
                Value::Quat([0.1, 0.2, 0.3, 0.927]),
                Value::UInt(200),
                Value::String("héllo wörld".into()),
                Value::Int(-300),
                Value::F32(3.5),
            ],
        };
        let mut w = BitWriter::new();
        v.encode_full(c, &mut w).unwrap();
        // 3*15 + (2+30) + 8 + (4+8*8) + 16 + 32 = 201 bits
        assert!(w.bit_len() <= 210, "{}", w.bit_len());
        let buf = w.finish();
        let d = ComponentValue::decode_full(c, &mut BitReader::new(&buf)).unwrap();
        match (&d.fields[0], &d.fields[1], &d.fields[3]) {
            (Value::Vec3(p), Value::Quat(q), Value::String(n)) => {
                assert!((p[1] + 45.67).abs() < 0.006);
                let dot: f32 = q
                    .iter()
                    .zip([0.1, 0.2, 0.3, 0.927])
                    .map(|(a, b)| a * b)
                    .sum();
                assert!(dot > 0.999, "{dot}");
                assert_eq!(n, "héllo w");
            }
            _ => panic!(),
        }
        assert_eq!(d.fields[2], Value::UInt(200));
        assert_eq!(d.fields[4], Value::Int(-300));
        assert_eq!(d.fields[5], Value::F32(3.5));
    }

    #[test]
    fn delta_only_changed_fields() {
        let s = schema();
        let c = s.component("T").unwrap();
        let base = ComponentValue::zero(c);
        let mut cur = base.clone();
        cur.fields[2] = Value::UInt(7);
        let mut w = BitWriter::new();
        assert!(cur.encode_delta(&base, c, &mut w).unwrap());
        assert_eq!(w.bit_len(), 6 + 8);
        let buf = w.finish();
        let d = ComponentValue::decode_delta(&base, c, &mut BitReader::new(&buf)).unwrap();
        assert_eq!(d, cur);
        let mut w2 = BitWriter::new();
        assert!(!cur.encode_delta(&cur, c, &mut w2).unwrap());
        assert_eq!(w2.bit_len(), 0);
    }

    #[test]
    fn type_mismatch_is_error() {
        let s = schema();
        let c = s.component("T").unwrap();
        let mut v = ComponentValue::zero(c);
        v.fields[2] = Value::Bool(true);
        assert_eq!(
            v.encode_full(c, &mut BitWriter::new()).unwrap_err(),
            CodecError::Type(2)
        );
    }
}
