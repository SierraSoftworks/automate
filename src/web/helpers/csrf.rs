//! CSRF protection for the admin forms.
//!
//! Admin actions are state-changing `POST` requests submitted from
//! server-rendered forms. To prevent cross-site request forgery we embed a
//! signed, time-limited token in every form and verify it when the form is
//! submitted. Tokens are HMAC-SHA256 signatures over a random nonce and an
//! issue timestamp, so they cannot be forged by a third party that does not
//! know the signing secret, and they expire after a short window.
//!
//! The signing secret is taken from `web.admin.csrf_secret` when configured
//! (allowing tokens to remain valid across restarts); otherwise a random secret
//! is generated once per process, which is sufficient for a single instance.

use std::sync::OnceLock;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::config::AdminConfig;

type HmacSha256 = Hmac<Sha256>;

/// How long a generated token remains valid, in seconds (8 hours).
const TOKEN_TTL_SECONDS: i64 = 60 * 60 * 8;

/// A small amount of clock skew (in seconds) tolerated on the "issued in the
/// future" check.
const CLOCK_SKEW_SECONDS: i64 = 60;

/// The process-global random secret used when no `csrf_secret` is configured.
static RUNTIME_SECRET: OnceLock<[u8; 32]> = OnceLock::new();

fn runtime_secret() -> &'static [u8; 32] {
    RUNTIME_SECRET.get_or_init(|| {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes
    })
}

fn secret_bytes(admin: &AdminConfig) -> Vec<u8> {
    match &admin.csrf_secret {
        Some(secret) if !secret.is_empty() => secret.as_bytes().to_vec(),
        _ => runtime_secret().to_vec(),
    }
}

fn sign(secret: &[u8], ts: &str, nonce: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts keys of any length");
    mac.update(ts.as_bytes());
    mac.update(b".");
    mac.update(nonce.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Generates a fresh CSRF token for embedding in an admin form.
pub fn generate_token(admin: &AdminConfig) -> String {
    let secret = secret_bytes(admin);
    let ts = chrono::Utc::now().timestamp().to_string();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let mac = sign(&secret, &ts, &nonce);
    format!("{ts}.{nonce}.{mac}")
}

/// Verifies a CSRF token submitted with an admin form, returning `true` only
/// when the signature is valid and the token has not expired.
pub fn validate_token(admin: &AdminConfig, token: &str) -> bool {
    let mut parts = token.splitn(3, '.');
    let (Some(ts_str), Some(nonce), Some(mac_hex)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };

    let Ok(ts) = ts_str.parse::<i64>() else {
        return false;
    };

    let now = chrono::Utc::now().timestamp();
    if ts > now + CLOCK_SKEW_SECONDS || now - ts > TOKEN_TTL_SECONDS {
        return false;
    }

    let Ok(provided) = hex::decode(mac_hex) else {
        return false;
    };

    let secret = secret_bytes(admin);
    let mut mac = HmacSha256::new_from_slice(&secret).expect("HMAC accepts keys of any length");
    mac.update(ts_str.as_bytes());
    mac.update(b".");
    mac.update(nonce.as_bytes());
    // `verify_slice` performs a constant-time comparison.
    mac.verify_slice(&provided).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_secret(secret: Option<&str>) -> AdminConfig {
        AdminConfig {
            csrf_secret: secret.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn generated_tokens_validate() {
        let admin = config_with_secret(Some("test-secret"));
        let token = generate_token(&admin);
        assert!(validate_token(&admin, &token));
    }

    #[test]
    fn tokens_are_rejected_under_a_different_secret() {
        let issuer = config_with_secret(Some("secret-a"));
        let verifier = config_with_secret(Some("secret-b"));
        let token = generate_token(&issuer);
        assert!(!validate_token(&verifier, &token));
    }

    #[test]
    fn tampered_tokens_are_rejected() {
        let admin = config_with_secret(Some("test-secret"));
        let token = generate_token(&admin);

        let mut parts: Vec<&str> = token.split('.').collect();
        // Flip the nonce so the signature no longer matches.
        let tampered_nonce = format!("{}x", parts[1]);
        parts[1] = &tampered_nonce;
        let tampered = parts.join(".");

        assert!(!validate_token(&admin, &tampered));
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        let admin = config_with_secret(Some("test-secret"));
        assert!(!validate_token(&admin, "not-a-token"));
        assert!(!validate_token(&admin, "1.2"));
        assert!(!validate_token(&admin, ""));
    }

    #[test]
    fn expired_tokens_are_rejected() {
        let admin = config_with_secret(Some("test-secret"));
        let secret = secret_bytes(&admin);
        let issued = (chrono::Utc::now().timestamp() - TOKEN_TTL_SECONDS - 60).to_string();
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let mac = sign(&secret, &issued, &nonce);
        let token = format!("{issued}.{nonce}.{mac}");
        assert!(!validate_token(&admin, &token));
    }

    #[test]
    fn runtime_secret_is_used_when_unconfigured() {
        let admin = config_with_secret(None);
        let token = generate_token(&admin);
        assert!(validate_token(&admin, &token));
    }
}
