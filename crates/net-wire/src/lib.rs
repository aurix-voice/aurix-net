//! Schema-driven wire format: an IDL describes components, the codec bit-packs values with
//! quantization and encodes deltas against a baseline with a per-field change mask.
//! The schema hash travels in the handshake so client and server never disagree silently.

pub mod bits;
pub mod schema;
pub mod value;

pub use bits::{BitReader, BitWriter};
pub use schema::{Component, Field, FieldType, Schema, SchemaError};
pub use value::{ComponentValue, Value};
