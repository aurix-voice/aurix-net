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
    pub fn zero(component: &Component) -> Self {
        Self {
            fields: component
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
        writer: &mut BitWriter,
        ty: &FieldType,
        value: &Value,
        idx: usize,
    ) -> Result<(), CodecError> {
        match (ty, value) {
            (FieldType::Bool, Value::Bool(b)) => writer.write_bool(*b),
            (FieldType::UInt { bits }, Value::UInt(x)) => writer.write_uint(*x, *bits),
            (FieldType::Int { bits }, Value::Int(x)) => writer.write_int(*x, *bits),
            (FieldType::F32, Value::F32(x)) => writer.write_f32(*x),
            (FieldType::Quant { min, max, res }, Value::F32(x)) => {
                writer.write_quantized(*x, *min, *max, *res)
            }
            (FieldType::Vec2 { min, max, res }, Value::Vec2(a)) => {
                for x in a {
                    writer.write_quantized(*x, *min, *max, *res);
                }
            }
            (FieldType::Vec3 { min, max, res }, Value::Vec3(a)) => {
                for x in a {
                    writer.write_quantized(*x, *min, *max, *res);
                }
            }
            (FieldType::Quat, Value::Quat(q)) => write_quat(writer, *q),
            (FieldType::String { max }, Value::String(s)) => {
                let bytes = &s.as_bytes()[..s.len().min(*max as usize)];
                let bytes = truncate_utf8(bytes);
                writer.write_uint(bytes.len() as u64, len_bits(*max));
                writer.write_bytes(bytes);
            }
            (FieldType::Bytes { max }, Value::Bytes(b)) => {
                let bytes = &b[..b.len().min(*max as usize)];
                writer.write_uint(bytes.len() as u64, len_bits(*max));
                writer.write_bytes(bytes);
            }
            _ => return Err(CodecError::Type(idx)),
        }
        Ok(())
    }

    fn read_field(reader: &mut BitReader, ty: &FieldType) -> Result<Value, CodecError> {
        Ok(match ty {
            FieldType::Bool => Value::Bool(reader.read_bool()?),
            FieldType::UInt { bits } => Value::UInt(reader.read_uint(*bits)?),
            FieldType::Int { bits } => Value::Int(reader.read_int(*bits)?),
            FieldType::F32 => Value::F32(reader.read_f32()?),
            FieldType::Quant { min, max, res } => {
                Value::F32(reader.read_quantized(*min, *max, *res)?)
            }
            FieldType::Vec2 { min, max, res } => Value::Vec2([
                reader.read_quantized(*min, *max, *res)?,
                reader.read_quantized(*min, *max, *res)?,
            ]),
            FieldType::Vec3 { min, max, res } => Value::Vec3([
                reader.read_quantized(*min, *max, *res)?,
                reader.read_quantized(*min, *max, *res)?,
                reader.read_quantized(*min, *max, *res)?,
            ]),
            FieldType::Quat => Value::Quat(read_quat(reader)?),
            FieldType::String { max } => {
                let len = reader.read_uint(len_bits(*max))? as usize;
                Value::String(
                    String::from_utf8(reader.read_bytes(len)?).map_err(|_| CodecError::Utf8)?,
                )
            }
            FieldType::Bytes { max } => {
                let len = reader.read_uint(len_bits(*max))? as usize;
                Value::Bytes(reader.read_bytes(len)?)
            }
        })
    }

    /// All fields.
    pub fn encode_full(
        &self,
        component: &Component,
        writer: &mut BitWriter,
    ) -> Result<(), CodecError> {
        if self.fields.len() != component.fields.len() {
            return Err(CodecError::Arity);
        }
        for (i, (f, v)) in component.fields.iter().zip(&self.fields).enumerate() {
            Self::write_field(writer, &f.ty, v, i)?;
        }
        Ok(())
    }

    pub fn decode_full(component: &Component, reader: &mut BitReader) -> Result<Self, CodecError> {
        Ok(Self {
            fields: component
                .fields
                .iter()
                .map(|f| Self::read_field(reader, &f.ty))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Change mask (one bit per field) followed by changed fields only. Returns false when
    /// nothing changed (nothing written).
    pub fn encode_delta(
        &self,
        base: &Self,
        component: &Component,
        writer: &mut BitWriter,
    ) -> Result<bool, CodecError> {
        if self.fields.len() != component.fields.len()
            || base.fields.len() != component.fields.len()
        {
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
            writer.write_bool(*ch);
        }
        for (i, (f, v)) in component.fields.iter().zip(&self.fields).enumerate() {
            if changed[i] {
                Self::write_field(writer, &f.ty, v, i)?;
            }
        }
        Ok(true)
    }

    pub fn decode_delta(
        base: &Self,
        component: &Component,
        reader: &mut BitReader,
    ) -> Result<Self, CodecError> {
        let mut changed = Vec::with_capacity(component.fields.len());
        for _ in 0..component.fields.len() {
            changed.push(reader.read_bool()?);
        }
        let mut out = base.clone();
        for (i, field) in component.fields.iter().enumerate() {
            if changed[i] {
                out.fields[i] = Self::read_field(reader, &field.ty)?;
            }
        }
        Ok(out)
    }
}

fn len_bits(max: u32) -> u32 {
    (64 - (max as u64).leading_zeros()).max(1)
}

fn truncate_utf8(bytes: &[u8]) -> &[u8] {
    let mut len = bytes.len();
    while len > 0 && std::str::from_utf8(&bytes[..len]).is_err() {
        len -= 1;
    }
    &bytes[..len]
}

/// Smallest-three: drop the largest-magnitude component, send the other three in [-1/√2, 1/√2].
fn write_quat(writer: &mut BitWriter, quat: [f32; 4]) {
    let len = (quat.iter().map(|x| x * x).sum::<f32>()).sqrt();
    let quat = if len > 0.0 {
        quat.map(|x| x / len)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    let mut largest = 0;
    for (i, x) in quat.iter().enumerate().skip(1) {
        if x.abs() > quat[largest].abs() {
            largest = i;
        }
    }
    let sign = if quat[largest] < 0.0 { -1.0 } else { 1.0 };
    writer.write_uint(largest as u64, 2);
    let lim = std::f32::consts::FRAC_1_SQRT_2;
    for (i, x) in quat.iter().enumerate() {
        if i != largest {
            writer.write_quantized(
                x * sign,
                -lim,
                lim,
                2.0 * lim / ((1u32 << QUAT_BITS) - 1) as f32,
            );
        }
    }
}

fn read_quat(reader: &mut BitReader) -> Result<[f32; 4], Eof> {
    let largest = reader.read_uint(2)? as usize;
    let lim = std::f32::consts::FRAC_1_SQRT_2;
    let mut quat = [0.0f32; 4];
    let mut sum = 0.0;
    for (i, slot) in quat.iter_mut().enumerate() {
        if i != largest {
            let part =
                reader.read_quantized(-lim, lim, 2.0 * lim / ((1u32 << QUAT_BITS) - 1) as f32)?;
            *slot = part;
            sum += part * part;
        }
    }
    quat[largest] = (1.0 - sum).max(0.0).sqrt();
    Ok(quat)
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
        let schema = schema();
        let component = schema.component("T").unwrap();
        let value = ComponentValue {
            fields: vec![
                Value::Vec3([1.23, -45.67, 99.99]),
                Value::Quat([0.1, 0.2, 0.3, 0.927]),
                Value::UInt(200),
                Value::String("héllo wörld".into()),
                Value::Int(-300),
                Value::F32(3.5),
            ],
        };
        let mut writer = BitWriter::new();
        value.encode_full(component, &mut writer).unwrap();
        // 3*15 + (2+30) + 8 + (4+8*8) + 16 + 32 = 201 bits
        assert!(writer.bit_len() <= 210, "{}", writer.bit_len());
        let buf = writer.finish();
        let decoded = ComponentValue::decode_full(component, &mut BitReader::new(&buf)).unwrap();
        match (&decoded.fields[0], &decoded.fields[1], &decoded.fields[3]) {
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
        assert_eq!(decoded.fields[2], Value::UInt(200));
        assert_eq!(decoded.fields[4], Value::Int(-300));
        assert_eq!(decoded.fields[5], Value::F32(3.5));
    }

    #[test]
    fn delta_only_changed_fields() {
        let schema = schema();
        let component = schema.component("T").unwrap();
        let base = ComponentValue::zero(component);
        let mut cur = base.clone();
        cur.fields[2] = Value::UInt(7);
        let mut writer = BitWriter::new();
        assert!(cur.encode_delta(&base, component, &mut writer).unwrap());
        assert_eq!(writer.bit_len(), 6 + 8);
        let buf = writer.finish();
        let decoded =
            ComponentValue::decode_delta(&base, component, &mut BitReader::new(&buf)).unwrap();
        assert_eq!(decoded, cur);
        let mut w2 = BitWriter::new();
        assert!(!cur.encode_delta(&cur, component, &mut w2).unwrap());
        assert_eq!(w2.bit_len(), 0);
    }

    #[test]
    fn type_mismatch_is_error() {
        let schema = schema();
        let component = schema.component("T").unwrap();
        let mut value = ComponentValue::zero(component);
        value.fields[2] = Value::Bool(true);
        assert_eq!(
            value
                .encode_full(component, &mut BitWriter::new())
                .unwrap_err(),
            CodecError::Type(2)
        );
    }
}
