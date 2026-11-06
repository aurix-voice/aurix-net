//! IDL: text schema -> components with typed fields. Grammar (one statement per line,
//! `//` comments):
//!
//! ```text
//! component Transform {
//!     pos: vec3 @range(-4096, 4096, 0.01)
//!     rot: quat
//!     hp: u8
//!     name: string(24)
//!     flags: bits(5)
//!     speed: f32          // raw 32-bit
//!     team: i16
//! }
//! ```
//!
//! Components are numbered in declaration order; the hash covers the canonical text, so
//! renaming or reordering is a different schema.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldType {
    Bool,
    /// Unsigned with `bits` width (u8=8, u16=16, u32=32, u64=64, bits(n)=n).
    UInt {
        bits: u32,
    },
    /// Signed zigzag with `bits` width.
    Int {
        bits: u32,
    },
    /// Raw f32.
    F32,
    /// Quantized float.
    Quant {
        min: f32,
        max: f32,
        res: f32,
    },
    /// Three quantized floats.
    Vec3 {
        min: f32,
        max: f32,
        res: f32,
    },
    /// Two quantized floats.
    Vec2 {
        min: f32,
        max: f32,
        res: f32,
    },
    /// Unit quaternion, smallest-three with 10 bits per component.
    Quat,
    /// UTF-8 up to `max` bytes.
    String {
        max: u32,
    },
    /// Opaque bytes up to `max`.
    Bytes {
        max: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: FieldType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    pub id: u16,
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    pub components: Vec<Component>,
    /// blake3 of the canonical text, hex.
    pub hash: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemaError {
    #[error("line {line}: {msg}")]
    Parse { line: usize, msg: String },
    #[error("duplicate component {0}")]
    DuplicateComponent(String),
    #[error("duplicate field {0}.{1}")]
    DuplicateField(String, String),
    #[error("too many components (max 4096)")]
    TooMany,
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_type(spec: &str, line: usize) -> Result<FieldType, SchemaError> {
    let err = |msg: String| SchemaError::Parse { line, msg };
    let (base, attr) = match spec.find('@') {
        Some(i) => (spec[..i].trim(), Some(spec[i + 1..].trim())),
        None => (spec.trim(), None),
    };
    let range = |attr: Option<&str>| -> Result<(f32, f32, f32), SchemaError> {
        let a = attr.ok_or_else(|| err(format!("{base} needs @range(min, max, res)")))?;
        let inner = a
            .strip_prefix("range(")
            .and_then(|s| s.strip_suffix(')'))
            .ok_or_else(|| err(format!("bad attribute {a}")))?;
        let parts: Vec<f32> = inner
            .split(',')
            .map(|p| p.trim().parse::<f32>())
            .collect::<Result<_, _>>()
            .map_err(|_| err(format!("bad range numbers in {a}")))?;
        if parts.len() != 3 || parts[1] <= parts[0] || parts[2] <= 0.0 {
            return Err(err(format!(
                "range must be (min, max, res) with max>min, res>0: {a}"
            )));
        }
        Ok((parts[0], parts[1], parts[2]))
    };
    let paren = |name: &str| -> Result<Option<u32>, SchemaError> {
        if let Some(rest) = base.strip_prefix(name) {
            if rest.is_empty() {
                return Ok(None);
            }
            let n = rest
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .and_then(|s| s.trim().parse::<u32>().ok())
                .ok_or_else(|| err(format!("bad {name} size in {base}")))?;
            return Ok(Some(n));
        }
        Ok(None)
    };
    Ok(match base {
        "bool" => FieldType::Bool,
        "u8" => FieldType::UInt { bits: 8 },
        "u16" => FieldType::UInt { bits: 16 },
        "u32" => FieldType::UInt { bits: 32 },
        "u64" => FieldType::UInt { bits: 64 },
        "i8" => FieldType::Int { bits: 8 },
        "i16" => FieldType::Int { bits: 16 },
        "i32" => FieldType::Int { bits: 32 },
        "i64" => FieldType::Int { bits: 64 },
        "f32" if attr.is_none() => FieldType::F32,
        "f32" => {
            let (min, max, res) = range(attr)?;
            FieldType::Quant { min, max, res }
        }
        "vec3" => {
            let (min, max, res) = range(attr)?;
            FieldType::Vec3 { min, max, res }
        }
        "vec2" => {
            let (min, max, res) = range(attr)?;
            FieldType::Vec2 { min, max, res }
        }
        "quat" => FieldType::Quat,
        _ => {
            if let Some(n) = paren("bits")? {
                if n == 0 || n > 64 {
                    return Err(err("bits(n) needs 1..=64".into()));
                }
                return Ok(FieldType::UInt { bits: n });
            }
            if base.starts_with("bits") {
                return Err(err(format!("bad bits spec {base}")));
            }
            if let Some(n) = paren("string")? {
                return Ok(FieldType::String {
                    max: n.clamp(1, 65535),
                });
            }
            if let Some(n) = paren("bytes")? {
                return Ok(FieldType::Bytes {
                    max: n.clamp(1, 65535),
                });
            }
            return Err(err(format!("unknown type {base}")));
        }
    })
}

impl Schema {
    pub fn parse(text: &str) -> Result<Self, SchemaError> {
        let mut components: Vec<Component> = Vec::new();
        let mut cur: Option<Component> = None;
        let mut canonical = String::new();
        for (i, raw) in text.lines().enumerate() {
            let line = i + 1;
            let l = match raw.find("//") {
                Some(p) => &raw[..p],
                None => raw,
            }
            .trim();
            if l.is_empty() {
                continue;
            }
            if let Some(rest) = l.strip_prefix("component ") {
                if cur.is_some() {
                    return Err(SchemaError::Parse {
                        line,
                        msg: "missing '}'".into(),
                    });
                }
                let name = rest.trim_end_matches('{').trim();
                if !ident_ok(name) {
                    return Err(SchemaError::Parse {
                        line,
                        msg: format!("bad component name {name}"),
                    });
                }
                if components.iter().any(|c| c.name == name) {
                    return Err(SchemaError::DuplicateComponent(name.to_string()));
                }
                if components.len() >= 4096 {
                    return Err(SchemaError::TooMany);
                }
                canonical.push_str(&format!("component {name} {{\n"));
                cur = Some(Component {
                    id: components.len() as u16,
                    name: name.to_string(),
                    fields: Vec::new(),
                });
                continue;
            }
            if l == "}" {
                let c = cur.take().ok_or(SchemaError::Parse {
                    line,
                    msg: "unexpected '}'".into(),
                })?;
                canonical.push_str("}\n");
                components.push(c);
                continue;
            }
            let c = cur.as_mut().ok_or(SchemaError::Parse {
                line,
                msg: "field outside component".into(),
            })?;
            let (name, spec) = l.split_once(':').ok_or(SchemaError::Parse {
                line,
                msg: format!("expected 'name: type', got {l}"),
            })?;
            let name = name.trim();
            if !ident_ok(name) {
                return Err(SchemaError::Parse {
                    line,
                    msg: format!("bad field name {name}"),
                });
            }
            if c.fields.iter().any(|f| f.name == name) {
                return Err(SchemaError::DuplicateField(
                    c.name.clone(),
                    name.to_string(),
                ));
            }
            let ty = parse_type(spec.trim(), line)?;
            canonical.push_str(&format!("  {name}: {ty:?}\n"));
            c.fields.push(Field {
                name: name.to_string(),
                ty,
            });
        }
        if cur.is_some() {
            return Err(SchemaError::Parse {
                line: text.lines().count(),
                msg: "missing '}'".into(),
            });
        }
        let hash = blake3::hash(canonical.as_bytes()).to_hex().to_string();
        Ok(Schema { components, hash })
    }

    pub fn component(&self, name: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.name == name)
    }

    pub fn by_id(&self, id: u16) -> Option<&Component> {
        self.components.get(id as usize)
    }

    /// Bits needed for a component id in this schema.
    pub fn component_id_bits(&self) -> u32 {
        (64 - (self.components.len().max(2) as u64 - 1).leading_zeros()).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
// movement
component Transform {
    pos: vec3 @range(-4096, 4096, 0.01)
    rot: quat
    speed: f32
}
component Health {
    hp: u8
    team: i16
    name: string(24)
    flags: bits(5)
}
"#;

    #[test]
    fn parses_and_hashes() {
        let s = Schema::parse(SRC).unwrap();
        assert_eq!(s.components.len(), 2);
        assert_eq!(s.component("Health").unwrap().id, 1);
        assert_eq!(
            s.component("Transform").unwrap().fields[0].ty,
            FieldType::Vec3 {
                min: -4096.0,
                max: 4096.0,
                res: 0.01
            }
        );
        assert_eq!(
            s.component("Health").unwrap().fields[3].ty,
            FieldType::UInt { bits: 5 }
        );
        let s2 = Schema::parse(&SRC.replace("hp: u8", "hp: u16")).unwrap();
        assert_ne!(s.hash, s2.hash);
        assert_eq!(
            Schema::parse(&format!("{SRC}\n// trailing comment"))
                .unwrap()
                .hash,
            s.hash
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            Schema::parse("component A {\n x: vec3\n}").unwrap_err(),
            SchemaError::Parse { .. }
        ));
        assert_eq!(
            Schema::parse("component A {\n}\ncomponent A {\n}").unwrap_err(),
            SchemaError::DuplicateComponent("A".into())
        );
        assert!(Schema::parse("component A {\n x: u8\n x: u8\n}").is_err());
        assert!(Schema::parse("component A {\n x: u8\n").is_err());
    }
}
