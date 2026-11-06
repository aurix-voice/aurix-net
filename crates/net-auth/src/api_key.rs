//! `an_<prefix8>_<secret>`: the prefix is stored in clear for display, the whole key only as
//! SHA-256. Keys are shown once at creation.

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Issue join grants, create/close rooms, read room lists.
    Rooms,
    /// Read usage and audit.
    Read,
    /// Manage keys, webhooks, schemas.
    Admin,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Rooms => "rooms",
            Scope::Read => "read",
            Scope::Admin => "admin",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rooms" => Some(Scope::Rooms),
            "read" => Some(Scope::Read),
            "admin" => Some(Scope::Admin),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiKeyMaterial {
    pub plaintext: String,
    pub prefix: String,
    pub hash: String,
}

impl ApiKeyMaterial {
    pub fn generate() -> Self {
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let prefix = secret[..8].to_string();
        let plaintext = format!("an_{prefix}_{}", &secret[8..]);
        let hash = hash_key(&plaintext);
        Self {
            plaintext,
            prefix,
            hash,
        }
    }
}

pub fn hash_key(plaintext: &str) -> String {
    hex::encode(Sha256::digest(plaintext.as_bytes()))
}

/// Cheap syntactic check before hitting the database.
pub fn looks_like_key(s: &str) -> bool {
    s.len() > 12 && s.starts_with("an_") && s.is_ascii()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_hash() {
        let k = ApiKeyMaterial::generate();
        assert!(looks_like_key(&k.plaintext));
        assert_eq!(k.hash, hash_key(&k.plaintext));
        assert_eq!(k.hash.len(), 64);
        assert_ne!(k.hash, ApiKeyMaterial::generate().hash);
    }
}
