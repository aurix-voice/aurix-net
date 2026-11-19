//! Join grants: the developer's backend asks the control plane for a short-lived token
//! naming the player and what they may do; the client presents it over the control channel.

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
    /// Decoding key of the secret being rotated out; accepted until the operator drops it.
    prev: Option<DecodingKey>,
    ttl_secs: i64,
}

impl GrantSigner {
    pub fn new(secret: &str, ttl_secs: u64) -> Self {
        Self::with_previous(secret, None, ttl_secs)
    }

    pub fn with_previous(secret: &str, previous: Option<&str>, ttl_secs: u64) -> Self {
        Self {
            enc: EncodingKey::from_secret(secret.as_bytes()),
            dec: DecodingKey::from_secret(secret.as_bytes()),
            prev: previous.map(|p| DecodingKey::from_secret(p.as_bytes())),
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
        if !valid_player_id(player_id) {
            return Err(NetError::Invalid(
                "player_id: 1..=128 chars, no control characters".into(),
            ));
        }
        if let Some(extra) = &meta {
            let size = serde_json::to_vec(extra)
                .map(|v| v.len())
                .unwrap_or(usize::MAX);
            if size > MAX_META_BYTES {
                return Err(NetError::Invalid(format!(
                    "meta must serialize to <= {MAX_META_BYTES} bytes"
                )));
            }
        }
        for scope in scopes {
            if !KNOWN_SCOPES.contains(scope) {
                return Err(NetError::Invalid(format!("unknown scope {scope}")));
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        if ttl_override == Some(0) {
            return Err(NetError::Invalid("ttl_secs must be at least 1".into()));
        }
        let ttl = ttl_override.map_or(self.ttl_secs, |t| {
            i64::try_from(t).map_or(self.ttl_secs, |t| t.min(self.ttl_secs))
        });
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
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 5;
        validation.validate_exp = true;
        validation.required_spec_claims.clear();
        let data = match jsonwebtoken::decode::<GrantClaims>(token, &self.dec, &validation) {
            Ok(decoded) => decoded,
            Err(e) if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::InvalidSignature) => {
                match &self.prev {
                    Some(prev) => jsonwebtoken::decode::<GrantClaims>(token, prev, &validation)
                        .map_err(|e| NetError::Unauthorized(format!("grant: {e}")))?,
                    None => return Err(NetError::Unauthorized(format!("grant: {e}"))),
                }
            }
            Err(e) => return Err(NetError::Unauthorized(format!("grant: {e}"))),
        };
        let claims = data.claims;
        let project = claims
            .pid
            .parse::<ProjectId>()
            .map_err(|_| NetError::Unauthorized("grant: bad project id".into()))?;
        Ok(Grant {
            player_id: claims.sub,
            project,
            name: claims.name,
            room: claims.room,
            scopes: claims.scopes,
            meta: claims.meta,
            jti: claims.jti,
            exp: claims.exp,
        })
    }
}

/// Whether `player_id` may name a player: 1 to 128 characters, none of them a control character.
pub fn valid_player_id(player_id: &str) -> bool {
    let len = player_id.chars().count();
    (1..=128).contains(&len) && !player_id.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_accepts_previous_secret_only_while_configured() {
        let old = GrantSigner::new("old-secret", 60);
        let (tok, _) = old
            .issue(ProjectId::new(), "p1", "Ann", None, &["join"], None, None)
            .unwrap();
        let rotated = GrantSigner::with_previous("new-secret", Some("old-secret"), 60);
        assert_eq!(rotated.verify(&tok).unwrap().player_id, "p1");
        let (fresh, _) = rotated
            .issue(ProjectId::new(), "p2", "Bob", None, &["join"], None, None)
            .unwrap();
        assert_eq!(rotated.verify(&fresh).unwrap().player_id, "p2");
        assert!(old.verify(&fresh).is_err());
        let dropped = GrantSigner::new("new-secret", 60);
        assert!(dropped.verify(&tok).is_err());
        let wrong = GrantSigner::with_previous("new-secret", Some("other"), 60);
        assert!(wrong.verify(&tok).is_err());
    }

    #[test]
    fn issue_verify() {
        let signer = GrantSigner::new("s3cret", 60);
        let project = ProjectId::new();
        let (tok, _) = signer
            .issue(
                project,
                "player-1",
                "Ann",
                Some("lobby"),
                &["join", "create"],
                None,
                None,
            )
            .unwrap();
        let grant = signer.verify(&tok).unwrap();
        assert_eq!(grant.player_id, "player-1");
        assert_eq!(grant.project, project);
        assert_eq!(grant.room.as_deref(), Some("lobby"));
        assert!(grant.can("join"));
        assert!(!grant.can("master"));
        assert!(GrantSigner::new("other", 60).verify(&tok).is_err());
    }

    #[test]
    fn player_id_is_measured_in_characters() {
        let signer = GrantSigner::new("s3cret", 60);
        let issue =
            |player: &str| signer.issue(ProjectId::new(), player, "", None, &[], None, None);
        assert!(issue(&"я".repeat(128)).is_ok());
        assert!(issue(&"я".repeat(129)).is_err());
        assert!(issue("").is_err());
        assert!(issue("a\nb").is_err());
    }

    #[test]
    fn ttl_is_capped() {
        let signer = GrantSigner::new("s3cret", 60);
        let (_, claims) = signer
            .issue(ProjectId::new(), "p", "", None, &[], None, Some(10_000))
            .unwrap();
        assert!(claims.exp - claims.iat <= 60);
    }

    #[test]
    fn ttl_too_large_for_a_signed_number_is_capped_and_zero_is_refused() {
        let signer = GrantSigner::new("s3cret", 60);
        for ttl in [1 << 63, u64::MAX] {
            let (_, claims) = signer
                .issue(ProjectId::new(), "p", "", None, &[], None, Some(ttl))
                .unwrap();
            assert_eq!(claims.exp - claims.iat, 60);
        }
        assert!(signer
            .issue(ProjectId::new(), "p", "", None, &[], None, Some(0))
            .is_err());
    }
}
