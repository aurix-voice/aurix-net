//! Join grants: the developer's backend asks the control plane for a short-lived token
//! naming the player and what they may do; the client presents it over the control channel.

use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use net_common::{NetError, ProjectId, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GrantClaims {
    /// Player id chosen by the developer (stable per user).
    pub sub: String,
    /// Project id.
    pub pid: String,
    /// Display name shown to other players.
    #[serde(default)]
    pub name: String,
    /// If set, the grant only admits to this room name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// `join`, `create`, `master`.
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
    pub exp: i64,
    pub iat: i64,
    pub jti: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub player_id: String,
    pub project: ProjectId,
    pub name: String,
    pub room: Option<String>,
    pub scopes: Vec<String>,
    pub meta: Option<serde_json::Value>,
    pub jti: String,
    pub exp: i64,
}

impl Grant {
    pub fn can(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

/// Largest `meta` payload a grant may carry (the grant travels in the first WS message).
pub const MAX_META_BYTES: usize = 1024;
pub const KNOWN_SCOPES: &[&str] = &["join", "create", "master", "spectate"];

#[derive(Clone)]
pub struct GrantSigner {
    enc: EncodingKey,
    dec: DecodingKey,
    ttl_secs: i64,
}

impl GrantSigner {
    pub fn new(secret: &str, ttl_secs: u64) -> Self {
        Self {
            enc: EncodingKey::from_secret(secret.as_bytes()),
            dec: DecodingKey::from_secret(secret.as_bytes()),
            ttl_secs: ttl_secs as i64,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        &self,
        project: ProjectId,
        player_id: &str,
        name: &str,
        room: Option<&str>,
        scopes: &[&str],
        meta: Option<serde_json::Value>,
        ttl_override: Option<u64>,
    ) -> Result<(String, GrantClaims)> {
        if player_id.is_empty() || player_id.len() > 128 {
            return Err(NetError::Invalid("player_id must be 1..=128 chars".into()));
        }
        if player_id.chars().any(char::is_control) {
            return Err(NetError::Invalid("player_id has control characters".into()));
        }
        if let Some(m) = &meta {
            let n = serde_json::to_vec(m).map(|v| v.len()).unwrap_or(usize::MAX);
            if n > MAX_META_BYTES {
                return Err(NetError::Invalid(format!(
                    "meta must serialize to <= {MAX_META_BYTES} bytes"
                )));
            }
        }
        for s in scopes {
            if !KNOWN_SCOPES.contains(s) {
                return Err(NetError::Invalid(format!("unknown scope {s}")));
            }
        }
        let now = Utc::now().timestamp();
        let ttl = ttl_override
            .map(|t| (t as i64).min(self.ttl_secs))
            .unwrap_or(self.ttl_secs);
        let claims = GrantClaims {
            sub: player_id.to_string(),
            pid: project.to_string(),
            name: name.chars().filter(|c| !c.is_control()).take(64).collect(),
            room: room.map(|r| r.to_string()),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            meta,
            exp: now + ttl,
            iat: now,
            jti: uuid::Uuid::new_v4().to_string(),
        };
        let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.enc)
            .map_err(|e| NetError::Internal(format!("sign grant: {e}")))?;
        Ok((token, claims))
    }

    pub fn verify(&self, token: &str) -> Result<Grant> {
        let mut v = Validation::new(Algorithm::HS256);
        v.leeway = 5;
        v.validate_exp = true;
        v.required_spec_claims.clear();
        let data = jsonwebtoken::decode::<GrantClaims>(token, &self.dec, &v)
            .map_err(|e| NetError::Unauthorized(format!("grant: {e}")))?;
        let c = data.claims;
        let project = c
            .pid
            .parse::<ProjectId>()
            .map_err(|_| NetError::Unauthorized("grant: bad project id".into()))?;
        Ok(Grant {
            player_id: c.sub,
            project,
            name: c.name,
            room: c.room,
            scopes: c.scopes,
            meta: c.meta,
            jti: c.jti,
            exp: c.exp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_verify() {
        let s = GrantSigner::new("s3cret", 60);
        let p = ProjectId::new();
        let (tok, _) = s
            .issue(
                p,
                "player-1",
                "Ann",
                Some("lobby"),
                &["join", "create"],
                None,
                None,
            )
            .unwrap();
        let g = s.verify(&tok).unwrap();
        assert_eq!(g.player_id, "player-1");
        assert_eq!(g.project, p);
        assert_eq!(g.room.as_deref(), Some("lobby"));
        assert!(g.can("join"));
        assert!(!g.can("master"));
        assert!(GrantSigner::new("other", 60).verify(&tok).is_err());
    }

    #[test]
    fn ttl_is_capped() {
        let s = GrantSigner::new("s3cret", 60);
        let (_, c) = s
            .issue(ProjectId::new(), "p", "", None, &[], None, Some(10_000))
            .unwrap();
        assert!(c.exp - c.iat <= 60);
    }
}
