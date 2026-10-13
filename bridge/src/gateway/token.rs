//! **THE TURN TOKEN** — what the TCP listener admits a request on.
//!
//! An HMAC-SHA256 over the turn's claims, under a key generated when the gateway is built and
//! held only in this process's memory: a restart invalidates every token, which is the right
//! fate for a token whose turn the restart also ended. The claims are the job id, the
//! capability, the granted server set, a tier (empty in this step; a later step fills it) and
//! an expiry of the turn deadline plus [`TOKEN_GRACE_SECS`].
//!
//! The wire form is `v1.<hex claims json>.<hex mac>`. The claims are NOT secret (they name a
//! job and a list of servers) and are not encrypted; what the MAC prevents is a token for one
//! server, one turn or one deadline being stretched to another.
//!
//! [`TokenKey::verify`] checks, in order, the shape, the MAC (constant time), the expiry and
//! that the requested server is in the granted set. Revocation is the gateway's: a valid token
//! whose turn has ended is refused because the turn's session is gone.

use ring::hmac;
use serde::{Deserialize, Serialize};

/// Seconds a token outlives its turn's deadline, so a slow final call is not cut off by its
/// own credential before the turn's timeout fires.
pub const TOKEN_GRACE_SECS: u64 = 60;

/// What a token asserts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The bridge's job id for the turn.
    pub job: String,
    /// `basic`, `read` or `write`.
    pub cap: String,
    /// The servers this turn may reach through the gateway, sorted.
    pub servers: Vec<String>,
    /// Reserved for the tier a later step assigns. Empty today, and signed anyway, so filling
    /// it later is not a format change.
    pub tier: String,
    /// Unix seconds after which the token is refused.
    pub exp: u64,
}

/// Why a token was refused. Deliberately coarse on the wire (every refusal is a 401); the
/// variant is for the test and the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    BadSignature,
    Expired,
    ServerNotGranted,
}

/// The in-memory signing key.
pub struct TokenKey(hmac::Key);

impl std::fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenKey(<in memory>)")
    }
}

impl TokenKey {
    /// A fresh random key. Never written anywhere.
    pub fn generate() -> TokenKey {
        let rng = ring::rand::SystemRandom::new();
        let key = hmac::Key::generate(hmac::HMAC_SHA256, &rng)
            .expect("the system random source is available");
        TokenKey(key)
    }

    /// A key from fixed bytes. Tests only: a forged token is one signed by a key other than the
    /// gateway's, and this is how the test holds that other key.
    #[cfg(test)]
    pub fn from_bytes(bytes: &[u8]) -> TokenKey {
        TokenKey(hmac::Key::new(hmac::HMAC_SHA256, bytes))
    }

    /// Sign `claims`.
    pub fn sign(&self, claims: &Claims) -> String {
        let body = serde_json::to_vec(claims).expect("claims serialise");
        let tag = hmac::sign(&self.0, &body);
        format!("v1.{}.{}", hex(&body), hex(tag.as_ref()))
    }

    /// Check `token` for `server` at `now` (unix seconds). Returns the claims when every check
    /// passes.
    pub fn verify(&self, token: &str, server: &str, now: u64) -> Result<Claims, TokenError> {
        let mut parts = token.split('.');
        let (Some("v1"), Some(body), Some(tag), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        let body = unhex(body).ok_or(TokenError::Malformed)?;
        let tag = unhex(tag).ok_or(TokenError::Malformed)?;
        hmac::verify(&self.0, &body, &tag).map_err(|_| TokenError::BadSignature)?;
        let claims: Claims = serde_json::from_slice(&body).map_err(|_| TokenError::Malformed)?;
        if now >= claims.exp {
            return Err(TokenError::Expired);
        }
        if !claims.servers.iter().any(|s| s == server) {
            return Err(TokenError::ServerNotGranted);
        }
        Ok(claims)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Unix seconds now.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(exp: u64) -> Claims {
        Claims {
            job: "job-1".into(),
            cap: "write".into(),
            servers: vec!["github".into(), "slack".into()],
            tier: String::new(),
            exp,
        }
    }

    #[test]
    fn a_token_verifies_for_a_granted_server_until_it_expires() {
        let k = TokenKey::generate();
        let t = k.sign(&claims(1_000));
        assert_eq!(k.verify(&t, "slack", 999).unwrap(), claims(1_000));
        assert_eq!(k.verify(&t, "slack", 1_000), Err(TokenError::Expired));
    }

    #[test]
    fn a_token_is_scoped_to_its_servers() {
        let k = TokenKey::generate();
        let t = k.sign(&claims(1_000));
        assert_eq!(k.verify(&t, "github", 1).map(|c| c.job), Ok("job-1".into()));
        assert_eq!(
            k.verify(&t, "proxmox", 1),
            Err(TokenError::ServerNotGranted)
        );
    }

    #[test]
    fn a_token_signed_by_another_key_is_refused() {
        let ours = TokenKey::generate();
        let forged = TokenKey::from_bytes(b"not the gateway's key").sign(&claims(u64::MAX));
        assert_eq!(
            ours.verify(&forged, "slack", 1),
            Err(TokenError::BadSignature)
        );
    }

    #[test]
    fn edited_claims_break_the_signature() {
        let k = TokenKey::generate();
        let t = k.sign(&claims(1_000));
        let mut parts: Vec<String> = t.split('.').map(str::to_string).collect();
        let mut c = claims(u64::MAX);
        c.servers.push("proxmox".into());
        parts[1] = hex(&serde_json::to_vec(&c).unwrap());
        assert_eq!(
            k.verify(&parts.join("."), "proxmox", 1),
            Err(TokenError::BadSignature)
        );
    }

    #[test]
    fn garbage_is_malformed() {
        let k = TokenKey::generate();
        for t in ["", "v1", "v1.zz.zz", "v2.00.00", "v1.00.00.00", "Bearer x"] {
            assert_eq!(k.verify(t, "slack", 1), Err(TokenError::Malformed), "{t}");
        }
    }

    #[test]
    fn the_key_never_prints() {
        assert_eq!(
            format!("{:?}", TokenKey::generate()),
            "TokenKey(<in memory>)"
        );
    }
}
