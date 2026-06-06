//! OpenID Connect (OIDC) machinery shared by the admin authentication endpoints.
//!
//! This module holds the reusable parts of the server-driven OIDC flow used by
//! the admin login: discovery and JWKS fetching/caching, ID token validation,
//! the authorization-URL builder, the confidential token exchange, and claim
//! filtering. The HTTP endpoints and middleware that drive this machinery live
//! in [`crate::web::api`].
//!
//! Unlike a browser-driven flow, the agent performs the entire Authorization
//! Code + PKCE exchange itself: it generates the PKCE verifier, redirects the
//! browser to the provider, receives the authorization code on its own callback
//! endpoint, and exchanges it using the confidential client credentials. The
//! resulting ID token is stored in an `HttpOnly` session cookie rather than ever
//! being exposed to JavaScript.
//!
//! The provider's discovery document and signing keys (JWKS) are cached for an
//! hour via [`crate::db::Cache`] so that we avoid hitting the provider on every
//! request while still picking up key rotations in a timely fashion.

use actix_web::http::header::HeaderMap;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::config::OidcConfig;
use crate::filter::FilterValue;
use crate::prelude::*;

/// JWT claims which carry protocol/registered semantics and should not be
/// exposed to the ACL filter as user-meaningful attributes.
const EXCLUDED_CLAIMS: &[&str] = &[
    "exp",
    "nbf",
    "iat",
    "iss",
    "aud",
    "jti",
    "nonce",
    "at_hash",
    "c_hash",
    "azp",
    "auth_time",
];

const ADVICE_PROVIDER: &[&str] = &[
    "Ensure that the `web.admin.oidc.endpoint` points at a valid OIDC provider.",
    "Check that the provider is reachable from this server.",
];

/// The subset of the OIDC discovery document we rely upon.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct OidcDiscovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

/// The token endpoint response from the provider. Only the ID token is used by
/// the server-driven cookie flow.
#[derive(serde::Deserialize)]
struct ProviderTokenResponse {
    id_token: String,
}

/// A PKCE code verifier and its derived S256 challenge.
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

/// Encodes bytes using URL-safe base64 without padding (per RFC 7636).
fn base64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// Generates a PKCE verifier (high-entropy, URL-safe) and its S256 challenge.
pub fn generate_pkce() -> PkcePair {
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = base64url(Sha256::digest(verifier.as_bytes()).as_slice());
    PkcePair {
        verifier,
        challenge,
    }
}

/// Generates an opaque, high-entropy random token suitable for use as an OAuth
/// `state` value or a CSRF token.
pub fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Builds the provider's authorization endpoint URL for the Authorization Code +
/// PKCE flow.
pub fn authorize_url(
    oidc: &OidcConfig,
    discovery: &OidcDiscovery,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> Result<String, human_errors::Error> {
    let mut scopes = vec!["openid".to_string()];
    for scope in &oidc.scopes {
        if scope != "openid" {
            scopes.push(scope.clone());
        }
    }
    let scope = scopes.join(" ");

    let mut url = reqwest::Url::parse(&discovery.authorization_endpoint).wrap_system_err(
        "The OIDC provider advertised an invalid authorization endpoint.",
        ADVICE_PROVIDER,
    )?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &oidc.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &scope)
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");

    Ok(url.to_string())
}

/// A [`Filterable`] view over an admin request, exposing request metadata and
/// (optionally) validated OIDC claims to the ACL filter.
pub struct AdminRequestFilter<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub client_ip: Option<String>,
    pub headers: &'a HeaderMap,
    pub claims: Option<&'a serde_json::Map<String, serde_json::Value>>,
}

impl Filterable for AdminRequestFilter<'_> {
    fn get(&self, key: &str) -> FilterValue {
        match key {
            "method" => self.method.into(),
            "path" => self.path.into(),
            "client_ip" => self.client_ip.clone().into(),
            key if key.starts_with("headers.") => {
                let header_name = &key["headers.".len()..];
                match self.headers.get(header_name) {
                    Some(value) => match value.to_str() {
                        Ok(s) => s.into(),
                        Err(_) => FilterValue::Null,
                    },
                    None => FilterValue::Null,
                }
            }
            key if key.starts_with("claims.") => {
                let claim_name = &key["claims.".len()..];
                match self.claims.and_then(|c| c.get(claim_name)) {
                    Some(value) => FilterValue::from(value),
                    None => FilterValue::Null,
                }
            }
            _ => FilterValue::Null,
        }
    }
}

/// Fetches and caches the OIDC discovery document for the configured provider.
#[instrument("web.oidc.discovery", skip(services, oidc), err(Display))]
pub async fn discovery<S: Services>(
    services: &S,
    oidc: &OidcConfig,
) -> Result<OidcDiscovery, human_errors::Error> {
    let endpoint = oidc.endpoint.trim_end_matches('/').to_string();
    let fetch_endpoint = endpoint.clone();
    let http_client = services.http_client();

    services
        .cache()
        .cached(
            "oidc:discovery",
            endpoint,
            move || {
                Box::pin(async move {
                    let url = format!("{fetch_endpoint}/.well-known/openid-configuration");
                    let document: OidcDiscovery = http_client
                        .get(&url)
                        .send()
                        .await
                        .wrap_system_err(
                            "Failed to fetch the OIDC discovery document from the provider.",
                            ADVICE_PROVIDER,
                        )?
                        .error_for_status()
                        .wrap_system_err(
                            "The OIDC provider returned an error when fetching its discovery document.",
                            ADVICE_PROVIDER,
                        )?
                        .json()
                        .await
                        .wrap_system_err(
                            "Failed to parse the OIDC discovery document returned by the provider.",
                            ADVICE_PROVIDER,
                        )?;

                    Ok(document)
                })
            },
            chrono::Duration::hours(1),
        )
        .await
}

/// Fetches and caches the JSON Web Key Set used to verify token signatures.
#[instrument("web.oidc.jwks", skip(services, discovery), err(Display))]
async fn jwks<S: Services>(
    services: &S,
    discovery: &OidcDiscovery,
) -> Result<jsonwebtoken::jwk::JwkSet, human_errors::Error> {
    let jwks_uri = discovery.jwks_uri.clone();
    let fetch_uri = jwks_uri.clone();
    let http_client = services.http_client();

    services
        .cache()
        .cached(
            "oidc:jwks",
            jwks_uri,
            move || {
                Box::pin(async move {
                    let keys: jsonwebtoken::jwk::JwkSet = http_client
                        .get(&fetch_uri)
                        .send()
                        .await
                        .wrap_system_err(
                            "Failed to fetch the OIDC signing keys (JWKS) from the provider.",
                            ADVICE_PROVIDER,
                        )?
                        .error_for_status()
                        .wrap_system_err(
                            "The OIDC provider returned an error when fetching its signing keys.",
                            ADVICE_PROVIDER,
                        )?
                        .json()
                        .await
                        .wrap_system_err(
                            "Failed to parse the OIDC signing keys returned by the provider.",
                            ADVICE_PROVIDER,
                        )?;

                    Ok(keys)
                })
            },
            chrono::Duration::hours(1),
        )
        .await
}

/// Validates an ID token's signature and registered claims (`aud`, `iss`,
/// `exp`, `nbf`) and returns the decoded claim set on success. This is used both
/// when issuing a session (after a token exchange) and when authenticating an
/// incoming bearer token on the API.
#[instrument("web.oidc.validate", skip(services, oidc, token), err(Display))]
pub async fn validate_token<S: Services>(
    services: &S,
    oidc: &OidcConfig,
    token: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, human_errors::Error> {
    let discovery = discovery(services, oidc).await?;
    let key_set = jwks(services, &discovery).await?;

    verify_token(&oidc.client_id, &discovery.issuer, &key_set, token)
}

/// Verifies an ID token against a known JWKS, audience, and issuer. This is the
/// pure (non-fetching) core of [`validate_token`] so it can be exercised in
/// isolation by tests.
fn verify_token(
    client_id: &str,
    issuer: &str,
    key_set: &jsonwebtoken::jwk::JwkSet,
    token: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, human_errors::Error> {
    let header = jsonwebtoken::decode_header(token).wrap_user_err(
        "The admin access token could not be decoded.",
        &["Sign in again to obtain a fresh access token."],
    )?;

    // Reject symmetric algorithms outright: the keys published via JWKS are
    // asymmetric, and allowing HMAC algorithms would expose us to algorithm
    // confusion attacks where the public key is used as an HMAC secret.
    if matches!(
        header.alg,
        jsonwebtoken::Algorithm::HS256
            | jsonwebtoken::Algorithm::HS384
            | jsonwebtoken::Algorithm::HS512
    ) {
        return Err(human_errors::user(
            "The admin access token is signed with an unsupported algorithm.",
            &["The OIDC provider must sign ID tokens with an asymmetric algorithm (e.g. RS256)."],
        ));
    }

    let kid = header.kid.ok_or_else(|| {
        human_errors::user(
            "The admin access token does not identify a signing key.",
            &["Sign in again to obtain a fresh access token."],
        )
    })?;

    let jwk = key_set.find(&kid).ok_or_else(|| {
        human_errors::user(
            "The admin access token was signed with an unknown key.",
            &["Sign in again to obtain a fresh access token."],
        )
    })?;

    let decoding_key = jsonwebtoken::DecodingKey::from_jwk(jwk).wrap_system_err(
        "Failed to construct a verification key from the provider's JWKS.",
        ADVICE_PROVIDER,
    )?;

    let mut validation = jsonwebtoken::Validation::new(header.alg);
    validation.set_audience(&[client_id]);
    validation.set_issuer(&[issuer]);
    validation.validate_exp = true;
    validation.validate_nbf = true;

    let data = jsonwebtoken::decode::<serde_json::Map<String, serde_json::Value>>(
        token,
        &decoding_key,
        &validation,
    )
    .wrap_user_err(
        "The admin access token failed validation.",
        &["Sign in again to obtain a fresh access token."],
    )?;

    Ok(data.claims)
}

/// Exchanges an authorization code (plus its PKCE verifier) for tokens at the
/// provider's token endpoint and returns the issued ID token. The confidential
/// client credentials are supplied from configuration so the secret never
/// leaves the server.
pub async fn exchange_code(
    oidc: &OidcConfig,
    discovery: &OidcDiscovery,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    http_client: &reqwest::Client,
) -> Result<String, human_errors::Error> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", code_verifier),
        ("redirect_uri", redirect_uri),
        ("client_id", oidc.client_id.as_str()),
        ("client_secret", oidc.client_secret.as_str()),
    ];

    let response: ProviderTokenResponse = http_client
        .post(&discovery.token_endpoint)
        .form(&params)
        .send()
        .await
        .wrap_system_err(
            "Failed to exchange the authorization code with the OIDC provider.",
            ADVICE_PROVIDER,
        )?
        .error_for_status()
        .wrap_user_err(
            "The OIDC provider rejected the authorization code exchange.",
            &["Start the sign-in process again from the beginning."],
        )?
        .json()
        .await
        .wrap_system_err(
            "Failed to parse the token response from the OIDC provider.",
            ADVICE_PROVIDER,
        )?;

    Ok(response.id_token)
}

/// Derives the [`automate_api::AdminUser`] display identity from a validated
/// claim set, falling back through the common OIDC name claims.
pub fn admin_user_from_claims(
    claims: &serde_json::Map<String, serde_json::Value>,
) -> automate_api::AdminUser {
    let str_claim = |key: &str| {
        claims
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };

    let email = str_claim("email");
    let name = str_claim("name")
        .or_else(|| str_claim("preferred_username"))
        .or_else(|| email.clone())
        .or_else(|| str_claim("sub"))
        .unwrap_or_else(|| "Signed in".to_string());

    automate_api::AdminUser { name, email }
}

/// Removes registered/temporal claims so the ACL filter only sees
/// user-meaningful attributes.
pub fn filterable_claims(
    claims: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    claims
        .iter()
        .filter(|(k, _)| !EXCLUDED_CLAIMS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::header::{HeaderMap, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                actix_web::http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn filterable_claims_strips_registered_claims() {
        let mut claims = serde_json::Map::new();
        claims.insert("sub".into(), serde_json::json!("user-1"));
        claims.insert("email".into(), serde_json::json!("a@example.com"));
        claims.insert("groups".into(), serde_json::json!(["admins", "users"]));
        claims.insert("exp".into(), serde_json::json!(123));
        claims.insert("iss".into(), serde_json::json!("https://idp"));
        claims.insert("aud".into(), serde_json::json!("client"));
        claims.insert("nonce".into(), serde_json::json!("abc"));

        let filtered = filterable_claims(&claims);
        assert!(filtered.contains_key("sub"));
        assert!(filtered.contains_key("email"));
        assert!(filtered.contains_key("groups"));
        assert!(!filtered.contains_key("exp"));
        assert!(!filtered.contains_key("iss"));
        assert!(!filtered.contains_key("aud"));
        assert!(!filtered.contains_key("nonce"));
    }

    #[test]
    fn admin_user_prefers_name_then_falls_back() {
        let mut claims = serde_json::Map::new();
        claims.insert("sub".into(), serde_json::json!("user-1"));
        let user = admin_user_from_claims(&claims);
        assert_eq!(user.name, "user-1");
        assert_eq!(user.email, None);

        claims.insert("email".into(), serde_json::json!("a@example.com"));
        let user = admin_user_from_claims(&claims);
        assert_eq!(user.name, "a@example.com");
        assert_eq!(user.email.as_deref(), Some("a@example.com"));

        claims.insert("name".into(), serde_json::json!("Ada Lovelace"));
        let user = admin_user_from_claims(&claims);
        assert_eq!(user.name, "Ada Lovelace");
    }

    #[test]
    fn admin_request_filter_exposes_claims() {
        let mut claims = serde_json::Map::new();
        claims.insert("email".into(), serde_json::json!("a@example.com"));
        claims.insert("groups".into(), serde_json::json!(["admins"]));

        let hdrs = headers(&[("x-custom", "value")]);
        let filter = AdminRequestFilter {
            method: "GET",
            path: "/api/v1/db",
            client_ip: Some("127.0.0.1".to_string()),
            headers: &hdrs,
            claims: Some(&claims),
        };

        assert_eq!(filter.get("method"), FilterValue::String("GET".into()));
        assert_eq!(
            filter.get("claims.email"),
            FilterValue::String("a@example.com".into())
        );
        assert_eq!(
            filter.get("headers.x-custom"),
            FilterValue::String("value".into())
        );
        assert_eq!(filter.get("claims.missing"), FilterValue::Null);
        // groups is an array, so an `in`/`contains` check resolves against it
        assert!(
            filter
                .get("claims.groups")
                .contains(&FilterValue::String("admins".into()))
        );
    }

    fn hs256_token(kid: Option<&str>) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
        header.kid = kid.map(|k| k.to_string());
        let claims = serde_json::json!({
            "sub": "user-1",
            "aud": "client",
            "iss": "https://idp.example.com",
            "exp": chrono::Utc::now().timestamp() + 3600,
        });
        jsonwebtoken::encode(
            &header,
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(b"super-secret"),
        )
        .unwrap()
    }

    #[test]
    fn verify_token_rejects_symmetric_algorithms() {
        // An attacker who knows the (public) JWKS could attempt an algorithm
        // confusion attack by signing an HS256 token. We must reject it before
        // ever attempting verification.
        let keys = jsonwebtoken::jwk::JwkSet { keys: vec![] };
        let token = hs256_token(Some("any"));
        let result = verify_token("client", "https://idp.example.com", &keys, &token);
        assert!(result.is_err());
    }

    #[test]
    fn verify_token_rejects_tokens_without_a_key_id() {
        let keys = jsonwebtoken::jwk::JwkSet { keys: vec![] };
        // Use an asymmetric alg in the header so we pass the symmetric guard and
        // reach the missing-kid check.
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        let claims = serde_json::json!({ "sub": "user-1" });
        // We can't sign RS256 without a key, so craft the token segments manually
        // since verify_token only needs to decode the header to read the kid.
        use base64::Engine;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header_segment = engine.encode(serde_json::to_vec(&header).unwrap());
        let claims_segment = engine.encode(serde_json::to_vec(&claims).unwrap());
        let token = format!("{header_segment}.{claims_segment}.sig");
        let result = verify_token("client", "https://idp.example.com", &keys, &token);
        assert!(result.is_err());
    }

    #[test]
    fn verify_token_rejects_malformed_tokens() {
        let keys = jsonwebtoken::jwk::JwkSet { keys: vec![] };
        let result = verify_token("client", "https://idp.example.com", &keys, "not-a-jwt");
        assert!(result.is_err());
    }
}
