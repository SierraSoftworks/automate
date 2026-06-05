//! OpenID Connect (OIDC) machinery shared by the admin authentication endpoints.
//!
//! This module holds the reusable parts of the OIDC flow: discovery and JWKS
//! fetching/caching, ID token validation, login-transaction cookies, claim
//! filtering, and the redirect that begins the login handshake. The HTTP
//! endpoints and middleware that drive this machinery live in
//! [`crate::web::oidc`].
//!
//! The provider's discovery document and signing keys (JWKS) are cached for an
//! hour via [`crate::db::Cache`] so that we avoid hitting the provider on every
//! request while still picking up key rotations in a timely fashion.

use actix_web::{
    HttpResponse,
    cookie::{Cookie, SameSite, time::Duration as CookieDuration},
    http::header::{self, HeaderMap},
};

use crate::config::OidcConfig;
use crate::filter::FilterValue;
use crate::prelude::*;
use crate::web::ui::error_page;

use super::request::{base_url, is_https};

/// The cookie used to persist an authenticated admin session (the ID token).
pub const SESSION_COOKIE: &str = "automate_admin_session";

/// The cookie used to carry the in-progress login transaction (CSRF state,
/// nonce and the original destination) between the authorization redirect and
/// the callback.
pub const LOGIN_COOKIE: &str = "automate_admin_login";

/// The path suffix of the OIDC callback endpoint within the admin scope.
pub const CALLBACK_PATH_SUFFIX: &str = "/.oidc/callback";

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
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

/// The token endpoint response (we only care about the ID token).
#[derive(serde::Deserialize)]
struct TokenResponse {
    id_token: String,
}

/// The state persisted in the login cookie for the duration of the OIDC
/// handshake.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct LoginTransaction {
    pub state: String,
    pub nonce: String,
    pub return_to: String,
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

/// Computes the base URL used to construct the OIDC redirect URI, preferring the
/// OIDC-specific `base_url`, then `web.base_url`, then the request host.
pub fn redirect_base_url<S: Services>(
    services: &S,
    oidc: &OidcConfig,
    headers: &HeaderMap,
    uri_scheme: Option<&str>,
) -> Option<String> {
    if let Some(base_url) = &oidc.base_url {
        return Some(base_url.trim_end_matches('/').to_string());
    }

    base_url(services, headers, uri_scheme)
}

/// Builds a cookie with secure-by-default attributes scoped to the admin area.
pub fn build_cookie<'c>(
    name: &'c str,
    value: String,
    secure: bool,
    max_age: CookieDuration,
) -> Cookie<'c> {
    Cookie::build(name, value)
        .path("/admin")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(max_age)
        .finish()
}

/// Builds a cookie which immediately expires the named cookie.
pub fn clear_cookie<'c>(name: &'c str, secure: bool) -> Cookie<'c> {
    build_cookie(name, String::new(), secure, CookieDuration::seconds(0))
}

/// Fetches and caches the OIDC discovery document for the configured provider.
#[instrument("web.oidc.discovery", skip(services, oidc), err(Display))]
pub async fn discovery<S: Services>(
    services: &S,
    oidc: &OidcConfig,
) -> Result<OidcDiscovery, human_errors::Error> {
    let endpoint = oidc.endpoint.trim_end_matches('/').to_string();
    let fetch_endpoint = endpoint.clone();

    services
        .cache()
        .cached(
            "oidc:discovery",
            endpoint,
            move || {
                Box::pin(async move {
                    let url = format!("{fetch_endpoint}/.well-known/openid-configuration");
                    let document: OidcDiscovery = reqwest::Client::new()
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

    services
        .cache()
        .cached(
            "oidc:jwks",
            jwks_uri,
            move || {
                Box::pin(async move {
                    let keys: jsonwebtoken::jwk::JwkSet = reqwest::Client::new()
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
/// `exp`, `nbf`) and returns the decoded claim set on success.
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
        "The admin session token could not be decoded.",
        &["Sign in again to obtain a fresh session token."],
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
            "The admin session token is signed with an unsupported algorithm.",
            &["The OIDC provider must sign ID tokens with an asymmetric algorithm (e.g. RS256)."],
        ));
    }

    let kid = header.kid.ok_or_else(|| {
        human_errors::user(
            "The admin session token does not identify a signing key.",
            &["Sign in again to obtain a fresh session token."],
        )
    })?;

    let jwk = key_set.find(&kid).ok_or_else(|| {
        human_errors::user(
            "The admin session token was signed with an unknown key.",
            &["Sign in again to obtain a fresh session token."],
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
        "The admin session token failed validation.",
        &["Sign in again to obtain a fresh session token."],
    )?;

    Ok(data.claims)
}

/// A minimal view of the signed-in administrator, derived from their ID token
/// claims, for display in the admin UI.
#[derive(Clone)]
pub struct AdminUser {
    /// A human-friendly display name (falls back through the common name
    /// claims).
    pub name: String,
    /// The user's email address, when the provider supplies one.
    pub email: Option<String>,
}

impl AdminUser {
    /// Derives a display identity from a validated claim set, falling back
    /// through the common OIDC name claims.
    pub fn from_claims(claims: &serde_json::Map<String, serde_json::Value>) -> Self {
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

        Self { name, email }
    }
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

/// Restricts a `return_to` destination to a local, absolute path to avoid open
/// redirect vulnerabilities.
pub fn safe_return_to(candidate: &str) -> String {
    // Reject anything that isn't a plain, absolute local path. Backslashes are
    // rejected because some browsers normalise them to `/`, turning `/\evil.com`
    // into a protocol-relative URL; control characters are rejected because they
    // can be used to smuggle past naive validation or split headers.
    let is_safe = candidate.starts_with('/')
        && !candidate.starts_with("//")
        && !candidate.contains('\\')
        && !candidate.chars().any(|c| c.is_control());

    if is_safe {
        candidate.to_string()
    } else {
        "/admin".to_string()
    }
}

/// Builds the redirect response which begins the OIDC login flow, setting the
/// login transaction cookie.
pub async fn begin_login<S: Services>(
    services: &S,
    oidc: &OidcConfig,
    headers: &HeaderMap,
    uri_scheme: Option<&str>,
    return_to: &str,
) -> HttpResponse {
    let discovery = match discovery(services, oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document: {e}");
            return error_page(
                502,
                "Bad Gateway",
                "We could not reach the configured identity provider.",
            )
            .await;
        }
    };

    let Some(base) = redirect_base_url(services, oidc, headers, uri_scheme) else {
        return error_page(
            400,
            "Bad Request",
            "Your request did not include a Host header, so we could not build the login redirect.",
        )
        .await;
    };

    let secure = is_https(services.config().web.trust_proxy, headers, uri_scheme);
    let state = uuid::Uuid::new_v4().simple().to_string();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let redirect_uri = format!("{base}/admin{CALLBACK_PATH_SUFFIX}");

    let mut scopes = vec!["openid".to_string()];
    for scope in &oidc.scopes {
        if scope != "openid" {
            scopes.push(scope.clone());
        }
    }

    let mut url = match reqwest::Url::parse(&discovery.authorization_endpoint) {
        Ok(url) => url,
        Err(e) => {
            error!("OIDC authorization endpoint is not a valid URL: {e}");
            return error_page(
                502,
                "Bad Gateway",
                "The identity provider advertised an invalid authorization endpoint.",
            )
            .await;
        }
    };

    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &oidc.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &scopes.join(" "))
        .append_pair("state", &state)
        .append_pair("nonce", &nonce);

    let transaction = LoginTransaction {
        state,
        nonce,
        return_to: safe_return_to(return_to),
    };

    let cookie_value = match serde_json::to_string(&transaction) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to serialise OIDC login transaction: {e}");
            return error_page(
                500,
                "Internal Server Error",
                "We could not start the login process. Please try again.",
            )
            .await;
        }
    };

    let login_cookie = build_cookie(
        LOGIN_COOKIE,
        cookie_value,
        secure,
        CookieDuration::minutes(10),
    );

    HttpResponse::Found()
        .cookie(login_cookie)
        .insert_header((header::LOCATION, url.to_string()))
        .finish()
}

/// Exchanges an authorization code for a token response at the provider's token
/// endpoint.
pub async fn exchange_code(
    oidc: &OidcConfig,
    discovery: &OidcDiscovery,
    code: &str,
    redirect_uri: &str,
) -> Result<String, human_errors::Error> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", oidc.client_id.as_str()),
        ("client_secret", oidc.client_secret.as_str()),
    ];

    let response: TokenResponse = reqwest::Client::new()
        .post(&discovery.token_endpoint)
        .form(&params)
        .send()
        .await
        .wrap_system_err(
            "Failed to exchange the authorization code with the OIDC provider.",
            ADVICE_PROVIDER,
        )?
        .error_for_status()
        .wrap_system_err(
            "The OIDC provider rejected the authorization code exchange.",
            ADVICE_PROVIDER,
        )?
        .json()
        .await
        .wrap_system_err(
            "Failed to parse the token response from the OIDC provider.",
            ADVICE_PROVIDER,
        )?;

    Ok(response.id_token)
}

/// Determines the cookie max-age from the token's `exp` claim, clamped to a
/// sensible bound.
pub fn session_max_age(claims: &serde_json::Map<String, serde_json::Value>) -> CookieDuration {
    let now = chrono::Utc::now().timestamp();
    let exp = claims.get("exp").and_then(|v| v.as_i64()).unwrap_or(now);
    let seconds = (exp - now).clamp(0, 60 * 60 * 12);
    CookieDuration::seconds(seconds)
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
    fn safe_return_to_rejects_external_destinations() {
        assert_eq!(safe_return_to("/admin/db"), "/admin/db");
        assert_eq!(safe_return_to("https://evil.example.com"), "/admin");
        assert_eq!(safe_return_to("//evil.example.com"), "/admin");
        assert_eq!(safe_return_to("admin"), "/admin");
        // Backslashes can be normalised to `/` by browsers, yielding a
        // protocol-relative URL.
        assert_eq!(safe_return_to("/\\evil.example.com"), "/admin");
        // Control characters (newlines, NUL, etc.) are rejected outright.
        assert_eq!(safe_return_to("/admin\nSet-Cookie: x=1"), "/admin");
        assert_eq!(safe_return_to("/admin\u{0000}"), "/admin");
    }

    #[test]
    fn admin_request_filter_exposes_claims() {
        let mut claims = serde_json::Map::new();
        claims.insert("email".into(), serde_json::json!("a@example.com"));
        claims.insert("groups".into(), serde_json::json!(["admins"]));

        let hdrs = headers(&[("x-custom", "value")]);
        let filter = AdminRequestFilter {
            method: "GET",
            path: "/admin/db",
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

    #[test]
    fn build_cookie_is_secure_by_default_attributes() {
        let cookie = build_cookie(
            SESSION_COOKIE,
            "token".into(),
            true,
            CookieDuration::seconds(60),
        );
        assert_eq!(cookie.path(), Some("/admin"));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));

        let insecure = build_cookie(
            SESSION_COOKIE,
            "token".into(),
            false,
            CookieDuration::seconds(60),
        );
        assert_eq!(insecure.secure(), Some(false));
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
