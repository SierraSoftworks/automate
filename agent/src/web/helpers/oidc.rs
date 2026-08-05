//! OpenID Connect (OIDC) machinery shared by the admin authentication endpoints.
//!
//! This module holds the reusable parts of the OIDC flow used by the admin
//! login: discovery and JWKS fetching/caching, ID token validation, the
//! confidential token exchange and refresh, and claim filtering. The HTTP
//! endpoints and middleware that drive this machinery live in
//! [`crate::web::api`].
//!
//! The browser runs the Authorization Code request itself (in a popup) and POSTs
//! the resulting `code` to the agent, which performs the confidential exchange
//! with its server-held `client_secret` and returns the ID token (the bearer the
//! SPA then presents on every API request) alongside a refresh token. PKCE is not
//! used — the agent is a confidential client, so the secret already binds the
//! exchange. The ID token is never set as a cookie; the SPA holds it in
//! `sessionStorage` and sends it as `Authorization: Bearer`.
//!
//! The provider's discovery document is cached for an hour via
//! [`crate::db::Cache`] to avoid hitting the provider on every request. The
//! signing keys (JWKS) are cached for longer (24 hours) because key rotations are
//! picked up on demand: [`validate_token`] refetches the JWKS when a token
//! presents an unrecognised `kid`.

use actix_web::http::header::HeaderMap;

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

/// Cache partition holding the provider's discovery document.
const DISCOVERY_CACHE_PARTITION: &str = "oidc:discovery";

/// Cache partition holding the provider's signing keys (JWKS).
const JWKS_CACHE_PARTITION: &str = "oidc:jwks";

/// How long the provider's signing keys (JWKS) are cached. Key rotations are
/// handled on demand — [`validate_token`] refetches when a token presents an
/// unknown `kid` — so a long TTL does not risk locking out valid sessions.
const JWKS_CACHE_TTL_HOURS: i64 = 24;

/// The subset of the OIDC discovery document we rely upon.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct OidcDiscovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

/// The token endpoint response from the provider. The `id_token` is the bearer
/// the agent validates; `refresh_token` (when issued) lets the SPA renew the
/// session without re-authenticating.
#[derive(serde::Deserialize)]
struct ProviderTokenResponse {
    id_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// Tokens issued by the OIDC token endpoint. `id_token` is the bearer presented
/// to the API; `refresh_token` (when present) renews the session.
#[derive(Clone)]
pub struct TokenSet {
    pub id_token: String,
    pub refresh_token: Option<String>,
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
    fn get(&self, key: &str) -> FilterValue<'_> {
        match key {
            "method" => self.method.into(),
            "path" => self.path.into(),
            "client_ip" => self.client_ip.as_deref().into(),
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
                    Some(value) => crate::filter::json_to_filter_value(value),
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
            DISCOVERY_CACHE_PARTITION,
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
///
/// When `force_refresh` is set, any cached key set is dropped first so the
/// provider is queried afresh. This is used when a token presents a `kid` we
/// don't recognise — typically because the provider has rotated its signing keys
/// since we last cached them — so that a rotation doesn't lock out otherwise
/// valid sessions until the cache expires.
#[instrument("web.oidc.jwks", skip(services, discovery), err(Display))]
async fn jwks<S: Services>(
    services: &S,
    discovery: &OidcDiscovery,
    force_refresh: bool,
) -> Result<jsonwebtoken::jwk::JwkSet, human_errors::Error> {
    let jwks_uri = discovery.jwks_uri.clone();

    if force_refresh {
        // `services.cache()` and `services.kv()` are the same store, so removing
        // the cache entry here forces the `cached` call below to rebuild it.
        services
            .kv()
            .remove(JWKS_CACHE_PARTITION, jwks_uri.clone())
            .await?;
    }

    let fetch_uri = jwks_uri.clone();
    let http_client = services.http_client();

    services
        .cache()
        .cached(
            JWKS_CACHE_PARTITION,
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
            chrono::Duration::hours(JWKS_CACHE_TTL_HOURS),
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
    let key_set = jwks(services, &discovery, false).await?;

    // If the token's signing key isn't in the cached JWKS, the provider may have
    // rotated its keys since we cached them. Refetch once, bypassing the cache,
    // before rejecting the token so that a key rotation doesn't lock out
    // otherwise valid sessions for the lifetime of the cache entry.
    let key_set = if needs_jwks_refresh(&key_set, token) {
        jwks(services, &discovery, true).await?
    } else {
        key_set
    };

    verify_token(&oidc.client_id, &discovery.issuer, &key_set, token)
}

/// Returns `true` when the token names a signing key (`kid`) that is absent from
/// the supplied key set, indicating the cached JWKS may be stale (for example
/// after the provider rotates its keys). A token that cannot be decoded, or one
/// without a `kid`, returns `false` so the verification path surfaces the real
/// error rather than triggering a pointless refetch.
fn needs_jwks_refresh(key_set: &jsonwebtoken::jwk::JwkSet, token: &str) -> bool {
    match jsonwebtoken::decode_header(token).ok().and_then(|h| h.kid) {
        Some(kid) => key_set.find(&kid).is_none(),
        None => false,
    }
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

    // Insisted on rather than merely checked when present. `jsonwebtoken`
    // compares `aud` and `iss` only against a token that carries them, and its
    // default requires nothing but `exp` — so a token with the audience simply
    // left out was accepted where one naming the wrong audience was refused.
    // That difference is only harmless while the provider issues tokens for
    // nothing but us; a provider shared with another application, omitting `aud`
    // for some of its tokens, is an ordinary thing to point this at.
    validation.set_required_spec_claims(&["exp", "aud", "iss"]);

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

/// Exchanges an authorization code for tokens at the provider's token endpoint.
/// The browser sends only the code; the confidential client credentials are
/// supplied from configuration so the secret never leaves the server. Returns the
/// `id_token` (used as the admin bearer) alongside the provider's `refresh_token`,
/// when one was issued.
pub async fn exchange_code(
    oidc: &OidcConfig,
    discovery: &OidcDiscovery,
    code: &str,
    redirect_uri: &str,
    http_client: &reqwest::Client,
) -> Result<TokenSet, human_errors::Error> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", oidc.client_id.as_str()),
        ("client_secret", oidc.client_secret.as_str()),
    ];

    token_request(
        http_client,
        &discovery.token_endpoint,
        &params,
        "The OIDC provider rejected the authorization code exchange.",
        &["Start the sign-in process again from the beginning."],
    )
    .await
}

/// Renews a session from a previously issued refresh token, returning a fresh ID
/// token (and a rotated refresh token when the provider supplies one). Providers
/// that don't rotate refresh tokens omit it from the response, so the caller's
/// token is carried over to keep the session renewable.
pub async fn refresh_tokens(
    oidc: &OidcConfig,
    discovery: &OidcDiscovery,
    refresh_token: &str,
    http_client: &reqwest::Client,
) -> Result<TokenSet, human_errors::Error> {
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", oidc.client_id.as_str()),
        ("client_secret", oidc.client_secret.as_str()),
    ];

    let mut tokens = token_request(
        http_client,
        &discovery.token_endpoint,
        &params,
        "The OIDC provider rejected the session renewal.",
        &["Sign in again to obtain a fresh session."],
    )
    .await?;
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token.to_string());
    }
    Ok(tokens)
}

/// POSTs a form-encoded grant to the provider's token endpoint and parses the
/// issued tokens.
async fn token_request(
    http_client: &reqwest::Client,
    token_endpoint: &str,
    params: &[(&str, &str)],
    rejection: &'static str,
    rejection_advice: &'static [&'static str],
) -> Result<TokenSet, human_errors::Error> {
    let response: ProviderTokenResponse = http_client
        .post(token_endpoint)
        .form(params)
        .send()
        .await
        .wrap_system_err(
            "Failed to reach the OIDC provider's token endpoint.",
            ADVICE_PROVIDER,
        )?
        .error_for_status()
        .wrap_user_err(rejection, rejection_advice)?
        .json()
        .await
        .wrap_system_err(
            "Failed to parse the token response from the OIDC provider.",
            ADVICE_PROVIDER,
        )?;

    Ok(TokenSet {
        id_token: response.id_token,
        refresh_token: response.refresh_token,
    })
}

/// Reads a non-empty string claim.
fn str_claim(claims: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    claims
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Derives the [`automate_api::AdminUser`] display identity from a validated
/// claim set, falling back through the common OIDC name claims.
///
/// Covers only how the person is presented. Which account they are — the name
/// their records are stored under — is [`username_from_claims`], which is
/// deliberately a separate decision: a display name may change freely, whereas
/// changing an account name moves everything somebody owns.
pub fn admin_user_from_claims(
    claims: &serde_json::Map<String, serde_json::Value>,
) -> automate_api::AdminUser {
    let email = str_claim(claims, "email");
    let name = str_claim(claims, "name")
        .or_else(|| str_claim(claims, "preferred_username"))
        .or_else(|| email.clone())
        .or_else(|| str_claim(claims, "sub"))
        .unwrap_or_else(|| "Signed in".to_string());

    automate_api::AdminUser {
        email,
        ..automate_api::AdminUser::new(name)
    }
}

/// Determines which account a validated claim set belongs to.
///
/// Uses the configured claim where one is named, otherwise `preferred_username`
/// falling back to `sub`. `sub` is the last resort rather than the default
/// because it is usually an opaque identifier, and an account name is something
/// people have to recognise, type into an impersonation header, and see in the
/// audit log.
pub fn username_from_claims(
    claims: &serde_json::Map<String, serde_json::Value>,
    configured_claim: Option<&str>,
) -> Result<TenantId, human_errors::Error> {
    let (claim, value) = match configured_claim {
        Some(claim) => (claim, str_claim(claims, claim)),
        None => (
            "preferred_username",
            str_claim(claims, "preferred_username").or_else(|| str_claim(claims, "sub")),
        ),
    };

    let Some(value) = value else {
        return Err(human_errors::user(
            format!(
                "Your identity provider did not supply a '{claim}' claim, so we cannot tell which account you are signing in to."
            ),
            &[
                "Configure your identity provider to include this claim in the ID token.",
                "Alternatively, set 'username_claim' under [web.auth.oidc] to a claim your provider does supply.",
            ],
        ));
    };

    TenantId::new(&value).map_err(|err| {
        human_errors::user(
            err.to_string(),
            &[
                "Point 'username_claim' under [web.auth.oidc] at a claim carrying a usable account name.",
            ],
        )
    })
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

    use crate::testing::oidc::{KEY_ID, TestIdentityProvider, unadvertised_key};

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

    /// Crafts a token whose header advertises an asymmetric algorithm and the
    /// given `kid`, with an unverifiable signature. This is enough to exercise the
    /// header-only `kid` inspection in [`needs_jwks_refresh`].
    fn rs256_token_with_kid(kid: &str) -> String {
        use base64::Engine;
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some(kid.to_string());
        let claims = serde_json::json!({ "sub": "user-1" });
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header_segment = engine.encode(serde_json::to_vec(&header).unwrap());
        let claims_segment = engine.encode(serde_json::to_vec(&claims).unwrap());
        format!("{header_segment}.{claims_segment}.sig")
    }

    #[test]
    fn needs_jwks_refresh_only_for_unknown_kid() {
        let empty = jsonwebtoken::jwk::JwkSet { keys: vec![] };

        // A token naming a key absent from the set should trigger a refresh.
        assert!(needs_jwks_refresh(&empty, &rs256_token_with_kid("rotated")));

        // A token we can't decode, or one without a `kid`, should not.
        assert!(!needs_jwks_refresh(&empty, "not-a-jwt"));
        assert!(!needs_jwks_refresh(&empty, &hs256_token(None)));
    }

    /// Builds a discovery document pointing every endpoint at the given mock server.
    fn mock_discovery(base: &str) -> OidcDiscovery {
        OidcDiscovery {
            issuer: base.to_string(),
            authorization_endpoint: format!("{base}/authorize"),
            token_endpoint: format!("{base}/token"),
            jwks_uri: format!("{base}/jwks"),
        }
    }

    fn test_oidc_config(base: &str) -> crate::config::OidcConfig {
        crate::config::OidcConfig {
            endpoint: base.to_string(),
            client_id: "test-client".into(),
            client_secret: "test-secret".into(),
            scopes: vec![],
            username_claim: None,
        }
    }

    #[tokio::test]
    async fn exchange_code_captures_the_refresh_token() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id_token": "header.payload.sig",
                "refresh_token": "refresh-123",
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let tokens = exchange_code(
            &test_oidc_config(&server.uri()),
            &mock_discovery(&server.uri()),
            "auth-code",
            "http://localhost/api/v1/auth/callback",
            &http,
        )
        .await
        .unwrap();
        assert_eq!(tokens.id_token, "header.payload.sig");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-123"));
    }

    #[tokio::test]
    async fn refresh_reuses_the_old_token_when_the_provider_does_not_rotate() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id_token": "renewed.id.token",
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let tokens = refresh_tokens(
            &test_oidc_config(&server.uri()),
            &mock_discovery(&server.uri()),
            "refresh-123",
            &http,
        )
        .await
        .unwrap();
        assert_eq!(tokens.id_token, "renewed.id.token");
        assert_eq!(
            tokens.refresh_token.as_deref(),
            Some("refresh-123"),
            "a non-rotating provider's response must not discard the caller's refresh token"
        );
    }

    #[tokio::test]
    async fn refresh_adopts_a_rotated_token_and_surfaces_rejections() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=live-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id_token": "renewed.id.token",
                "refresh_token": "rotated-456",
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=revoked-token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let oidc = test_oidc_config(&server.uri());
        let discovery = mock_discovery(&server.uri());

        let tokens = refresh_tokens(&oidc, &discovery, "live-token", &http)
            .await
            .unwrap();
        assert_eq!(tokens.refresh_token.as_deref(), Some("rotated-456"));

        assert!(
            refresh_tokens(&oidc, &discovery, "revoked-token", &http)
                .await
                .is_err(),
            "a rejected grant must surface as an error so the session is dropped"
        );
    }

    #[tokio::test]
    async fn validate_token_refetches_jwks_on_unknown_kid() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let discovery_body = serde_json::json!({
            "issuer": server.uri(),
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
            "jwks_uri": format!("{}/jwks", server.uri()),
        });
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(discovery_body))
            .mount(&server)
            .await;

        // The JWKS never contains the token's key. We expect it to be fetched
        // twice: once on the initial cache-miss read, and once more after the
        // unknown `kid` forces a cache-bypassing refetch. The `.expect(2)` is
        // verified when the server is dropped at the end of the test.
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "keys": [] })),
            )
            .expect(2)
            .mount(&server)
            .await;

        let services = crate::services::ServicesContainer::new_custom_mock(|c, _| {
            c.web.admin.oidc = Some(crate::config::OidcConfig {
                endpoint: server.uri(),
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                scopes: vec![],
                username_claim: None,
            })
        })
        .await
        .unwrap();
        let oidc = services.config().web.admin.oidc.clone().unwrap();

        let token = rs256_token_with_kid("rotated");
        let result = validate_token(&services, &oidc, &token).await;

        // Verification can't succeed against an empty JWKS, but the refetch must
        // have been attempted (asserted via the mock's expected hit count).
        assert!(result.is_err());
    }

    // Everything below drives [`validate_token`] against a real identity
    // provider, which is the only way any of it can be reached: until there was
    // something able to mint a token the agent would accept, every path through
    // here that ends in "yes" was unreachable, and the ones that end in "no"
    // could only be reached with tokens too malformed to get as far as a
    // signature check. See [`crate::testing::oidc`] for why the provider is real
    // rather than a `#[cfg(test)]` shortcut in this file.

    /// An agent configured to trust `provider`, held as the middleware holds it.
    ///
    /// The services are scoped to the installation's own account because that is
    /// where the middleware puts the discovery and JWKS caches.
    async fn agent_trusting(
        provider: &TestIdentityProvider,
    ) -> (crate::services::AppServices, OidcConfig) {
        let context = provider.context().await;
        let config = context.config();
        let oidc = config
            .web
            .oidc()
            .cloned()
            .expect("the context should trust the provider it was built from");

        (context.tenant(TenantId::local()), oidc)
    }

    #[tokio::test]
    async fn a_token_the_provider_signed_is_accepted_and_its_claims_come_back_intact() {
        // The positive control the refusals below rest on. Without it every one
        // of them would also be satisfied by a validator that refused
        // everything, which is not the property anybody wants.
        let provider = TestIdentityProvider::start().await;
        let (services, oidc) = agent_trusting(&provider).await;

        let claims = validate_token(&services, &oidc, &provider.sign_in_as("alice"))
            .await
            .expect("a token this provider signed should be accepted");

        assert_eq!(claims["preferred_username"], "alice");
        assert_eq!(claims["sub"], "subject-id-for-alice");
        assert_eq!(claims["email"], "alice@example.com");
    }

    #[tokio::test]
    async fn a_token_signed_by_a_key_the_provider_does_not_advertise_is_refused() {
        // The forgery that matters. The JWKS is public, so the `kid` naming a
        // legitimate key is not a secret and an attacker will reuse it — what
        // they cannot do is produce a signature that verifies against the
        // matching public half. Everything else about this token is correct:
        // right issuer, right audience, in date, naming a key the provider does
        // publish.
        let provider = TestIdentityProvider::start().await;
        let (services, oidc) = agent_trusting(&provider).await;

        let forged = provider.issue_signed_by(
            &unadvertised_key(),
            Some(KEY_ID),
            provider.claims_for("alice"),
        );

        assert!(
            validate_token(&services, &oidc, &forged).await.is_err(),
            "a token signed by anybody other than the provider must not sign somebody in",
        );
    }

    #[tokio::test]
    async fn a_token_naming_a_signing_key_the_provider_never_published_is_refused() {
        // The complement of the test above: this signature really is the
        // provider's, and only the `kid` is wrong. It has to be refused anyway,
        // because the `kid` is how the agent decides which public key to check
        // against — accepting a token whose key it cannot name would mean
        // accepting one it never actually verified.
        let provider = TestIdentityProvider::start().await;
        let (services, oidc) = agent_trusting(&provider).await;

        let mislabelled = provider.issue_with_kid(
            // Signed by the provider itself; only the label is a lie.
            Some("a-key-nobody-has-heard-of"),
            provider.claims_for("alice"),
        );

        assert!(
            validate_token(&services, &oidc, &mislabelled)
                .await
                .is_err(),
            "a token naming an unpublished key must be refused",
        );
    }

    #[tokio::test]
    async fn an_expired_token_is_refused() {
        // Sessions ending is the whole point of `exp`. An agent that ignored it
        // would turn every ID token it ever saw into a permanent credential, and
        // the only remedy left would be rotating the provider's signing key.
        let provider = TestIdentityProvider::start().await;
        let (services, oidc) = agent_trusting(&provider).await;

        let mut claims = provider.claims_for("alice");
        // Well beyond the 60 seconds of clock-skew leeway `jsonwebtoken` allows
        // by default, so this is testing expiry rather than the size of that
        // window.
        claims["exp"] = serde_json::json!(chrono::Utc::now().timestamp() - 3600);

        assert!(
            validate_token(&services, &oidc, &provider.issue(claims))
                .await
                .is_err(),
            "an expired session must not still let somebody in",
        );
    }

    #[tokio::test]
    async fn a_token_minted_for_another_audience_is_refused() {
        // The same provider may issue tokens to several applications. A token
        // the user obtained for a different one is genuinely signed, in date and
        // genuinely theirs — but it was never presented to them as a credential
        // for this agent, and treating it as one would let any other client of
        // the same provider sign people in here.
        let provider = TestIdentityProvider::start().await;
        let (services, oidc) = agent_trusting(&provider).await;

        let mut claims = provider.claims_for("alice");
        claims["aud"] = serde_json::json!("some-other-application");

        assert!(
            validate_token(&services, &oidc, &provider.issue(claims))
                .await
                .is_err(),
            "a token addressed to another application must not be accepted here",
        );
    }

    #[tokio::test]
    async fn a_token_that_simply_leaves_out_its_audience_is_refused_too() {
        // `jsonwebtoken` compares `aud` and `iss` only against a token that
        // carries them, so omitting one entirely used to be accepted where
        // naming the wrong one was refused. Harmless only while the provider
        // issues tokens for nobody but us, which is not something to depend on.
        let provider = TestIdentityProvider::start().await;
        let (services, oidc) = agent_trusting(&provider).await;

        let mut claims = provider.claims_for("alice");
        let stripped = claims.as_object_mut().unwrap();
        stripped.remove("aud");
        stripped.remove("iss");

        let token = provider.issue(claims);

        assert!(
            validate_token(&services, &oidc, &token).await.is_err(),
            "a token that names no audience is not one we were meant to be given",
        );
    }

    #[tokio::test]
    async fn the_signing_keys_are_fetched_once_and_an_unknown_key_forces_exactly_one_refetch() {
        // Both halves matter and they pull against each other. Fetching the JWKS
        // on every request would put the identity provider in the path of every
        // API call the browser makes; never refetching would mean a key rotation
        // locked everybody out for the 24 hours the cache lives. The compromise
        // is a long cache with a refetch triggered by an unrecognised `kid`, and
        // that only works if the refetch happens once — a token naming a key
        // that will never exist must not become a request to the provider every
        // time it is presented.
        let provider = TestIdentityProvider::start().await;
        let (services, oidc) = agent_trusting(&provider).await;

        validate_token(&services, &oidc, &provider.sign_in_as("alice"))
            .await
            .expect("the first sign-in should be accepted");
        assert_eq!(provider.jwks_fetches().await, 1);
        assert_eq!(provider.discovery_fetches().await, 1);

        validate_token(&services, &oidc, &provider.sign_in_as("alice"))
            .await
            .expect("the second sign-in should be accepted");
        assert_eq!(
            provider.jwks_fetches().await,
            1,
            "a second sign-in should be served from the cached key set",
        );

        let rotated = provider.issue_with_kid(
            Some("a-key-nobody-has-heard-of"),
            provider.claims_for("alice"),
        );
        assert!(validate_token(&services, &oidc, &rotated).await.is_err());
        assert_eq!(
            provider.jwks_fetches().await,
            2,
            "an unrecognised key should send the agent back to the provider exactly once",
        );

        // And the refetched key set is cached in turn, so the next ordinary
        // sign-in does not go back out to the provider either.
        validate_token(&services, &oidc, &provider.sign_in_as("alice"))
            .await
            .expect("a sign-in after the refetch should be accepted");
        assert_eq!(provider.jwks_fetches().await, 2);
        assert_eq!(provider.discovery_fetches().await, 1);
    }
}
