//! Server-driven OIDC session endpoints and the CSRF token endpoint.
//!
//! The agent performs the entire Authorization Code + PKCE exchange itself so
//! that the browser never handles tokens or the client secret. After a
//! successful exchange the issued ID token is stored in an `HttpOnly` session
//! cookie and presented automatically by the browser on subsequent same-origin
//! API requests. A separate double-submit CSRF token guards mutating requests.

use actix_web::cookie::time::Duration as CookieDuration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::http::header::LOCATION;
use actix_web::{HttpRequest, HttpResponse, web};
use serde::{Deserialize, Serialize};

use super::{CSRF_COOKIE, OAUTH_COOKIE, SESSION_COOKIE, json_error};
use crate::prelude::*;
use crate::web::helpers::oidc::{
    authorize_url, discovery, exchange_code, generate_pkce, random_token, validate_token,
};
use crate::web::helpers::request::{base_url, is_https};

/// The default lifetime applied to the session cookie when the provider does not
/// advertise an ID token expiry we can use.
const DEFAULT_SESSION_SECONDS: i64 = 8 * 60 * 60;

/// The lifetime of the short-lived cookie that carries the in-flight OAuth state
/// (PKCE verifier, CSRF `state`, and post-login destination) across the redirect
/// to the identity provider.
const OAUTH_STATE_SECONDS: i64 = 10 * 60;

/// The path the session and CSRF cookies are scoped to. They must be sent on
/// every API request, so they are rooted at the site origin.
const COOKIE_PATH: &str = "/";

/// The path the transient OAuth-state cookie is scoped to. It is only needed by
/// the callback endpoint, so it is narrowly scoped.
const OAUTH_COOKIE_PATH: &str = "/api/v1/auth";

/// The transient state persisted (in an `HttpOnly` cookie) across the redirect
/// to the identity provider so the callback can verify the response and complete
/// the PKCE exchange.
#[derive(Serialize, Deserialize)]
struct OAuthState {
    state: String,
    verifier: String,
    redirect_uri: String,
    return_to: String,
}

/// Query parameters supplied by the identity provider on the callback redirect.
#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Query parameters accepted by the login endpoint.
#[derive(Deserialize)]
pub struct LoginQuery {
    return_to: Option<String>,
}

/// `GET /api/v1/auth/login` — begins the server-driven OIDC login by redirecting
/// the browser to the identity provider with a freshly generated PKCE challenge.
pub async fn auth_login<S: Services>(
    services: web::Data<S>,
    req: HttpRequest,
    query: web::Query<LoginQuery>,
) -> HttpResponse {
    let config = services.config();
    let Some(oidc) = config.web.admin.oidc.as_ref() else {
        // Nothing to log in to; send the browser back to the app.
        return redirect_to("/");
    };

    let Some(base) = base_url(services.as_ref(), req.headers(), req.uri().scheme_str()) else {
        return json_error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "Could not determine the public base URL for the login redirect.",
        );
    };
    let redirect_uri = format!("{base}/api/v1/auth/callback");

    let discovery = match discovery(services.as_ref(), oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document during login: {e}");
            return json_error(
                actix_web::http::StatusCode::BAD_GATEWAY,
                "We could not reach the configured identity provider.",
            );
        }
    };

    let pkce = generate_pkce();
    let state = random_token();

    let authorize = match authorize_url(oidc, &discovery, &redirect_uri, &state, &pkce.challenge) {
        Ok(url) => url,
        Err(e) => {
            error!("Failed to build the OIDC authorization URL: {e}");
            return json_error(
                actix_web::http::StatusCode::BAD_GATEWAY,
                "We could not start the sign-in with the identity provider.",
            );
        }
    };

    let oauth_state = OAuthState {
        state,
        verifier: pkce.verifier,
        redirect_uri,
        return_to: sanitize_return_to(query.return_to.as_deref()),
    };
    let serialized = match serde_json::to_string(&oauth_state) {
        Ok(value) => value,
        Err(e) => {
            error!("Failed to serialize the OAuth state cookie: {e}");
            return json_error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "We could not start the sign-in process.",
            );
        }
    };

    let secure = is_https(
        config.web.trust_proxy,
        req.headers(),
        req.uri().scheme_str(),
    );
    let cookie = Cookie::build(OAUTH_COOKIE, serialized)
        .path(OAUTH_COOKIE_PATH)
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(CookieDuration::seconds(OAUTH_STATE_SECONDS))
        .finish();

    HttpResponse::Found()
        .cookie(cookie)
        .insert_header((LOCATION, authorize))
        .finish()
}

/// `GET /api/v1/auth/callback` — completes the OIDC exchange, sets the session
/// cookie, and redirects back into the app.
pub async fn auth_callback<S: Services>(
    services: web::Data<S>,
    req: HttpRequest,
    query: web::Query<CallbackQuery>,
) -> HttpResponse {
    let config = services.config();
    let secure = is_https(
        config.web.trust_proxy,
        req.headers(),
        req.uri().scheme_str(),
    );

    let Some(oidc) = config.web.admin.oidc.as_ref() else {
        return redirect_to("/");
    };

    if let Some(error) = query.error.as_deref() {
        warn!("The OIDC provider returned an error on the callback: {error}");
        return clear_oauth_and_redirect("/?auth_error=denied");
    }

    let (Some(code), Some(state)) = (query.code.as_deref(), query.state.as_deref()) else {
        return clear_oauth_and_redirect("/?auth_error=invalid");
    };

    let Some(oauth_state) = req
        .cookie(OAUTH_COOKIE)
        .and_then(|c| serde_json::from_str::<OAuthState>(c.value()).ok())
    else {
        return clear_oauth_and_redirect("/?auth_error=expired");
    };

    // The state is a public, single-use value whose only job is to bind the
    // callback to the browser that began the flow.
    if oauth_state.state != state {
        warn!("Rejected an OIDC callback whose state did not match the stored value.");
        return clear_oauth_and_redirect("/?auth_error=invalid");
    }

    let discovery = match discovery(services.as_ref(), oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document during callback: {e}");
            return clear_oauth_and_redirect("/?auth_error=provider");
        }
    };

    let id_token = match exchange_code(
        oidc,
        &discovery,
        code,
        &oauth_state.verifier,
        &oauth_state.redirect_uri,
        &services.http_client(),
    )
    .await
    {
        Ok(token) => token,
        Err(e) => {
            warn!("OIDC token exchange failed: {e}");
            return clear_oauth_and_redirect("/?auth_error=exchange");
        }
    };

    // Validate the freshly issued ID token before trusting it for a session, so
    // we fail fast on misconfiguration rather than handing out a cookie we would
    // later reject.
    let claims = match validate_token(services.as_ref(), oidc, &id_token).await {
        Ok(claims) => claims,
        Err(e) => {
            warn!("OIDC provider issued an ID token that failed validation: {e}");
            return clear_oauth_and_redirect("/?auth_error=token");
        }
    };

    let max_age = claims
        .get("exp")
        .and_then(|v| v.as_i64())
        .map(|exp| exp - chrono::Utc::now().timestamp())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_SESSION_SECONDS);

    let session_cookie = Cookie::build(SESSION_COOKIE, id_token)
        .path(COOKIE_PATH)
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(CookieDuration::seconds(max_age))
        .finish();

    let mut oauth_removal = Cookie::build(OAUTH_COOKIE, "")
        .path(OAUTH_COOKIE_PATH)
        .finish();
    oauth_removal.make_removal();

    HttpResponse::Found()
        .cookie(session_cookie)
        .cookie(oauth_removal)
        .insert_header((LOCATION, oauth_state.return_to))
        .finish()
}

/// `POST /api/v1/auth/logout` — clears the session cookie.
pub async fn auth_logout() -> HttpResponse {
    let mut removal = Cookie::build(SESSION_COOKIE, "").path(COOKIE_PATH).finish();
    removal.make_removal();

    HttpResponse::NoContent().cookie(removal).finish()
}

/// `GET /api/v1/csrf` — issues a double-submit CSRF token, returning it in the
/// body and setting the matching (non-`HttpOnly`) cookie.
pub async fn csrf_token<S: Services>(services: web::Data<S>, req: HttpRequest) -> HttpResponse {
    let secure = is_https(
        services.config().web.trust_proxy,
        req.headers(),
        req.uri().scheme_str(),
    );
    let token = random_token();

    let cookie = Cookie::build(CSRF_COOKIE, token.clone())
        .path(COOKIE_PATH)
        // Deliberately NOT HttpOnly: the SPA reads this value to echo it back in
        // the X-CSRF-Token header (the double-submit pattern).
        .http_only(false)
        .secure(secure)
        .same_site(SameSite::Lax)
        .finish();

    HttpResponse::Ok()
        .cookie(cookie)
        .json(automate_api::CsrfToken { token })
}

/// Builds a bare 302 redirect to the given location.
fn redirect_to(location: &str) -> HttpResponse {
    HttpResponse::Found()
        .insert_header((LOCATION, location))
        .finish()
}

/// Redirects to the given location while removing the transient OAuth-state
/// cookie (used on every callback failure path).
fn clear_oauth_and_redirect(location: &str) -> HttpResponse {
    let mut removal = Cookie::build(OAUTH_COOKIE, "")
        .path(OAUTH_COOKIE_PATH)
        .finish();
    removal.make_removal();

    HttpResponse::Found()
        .cookie(removal)
        .insert_header((LOCATION, location))
        .finish()
}

/// Ensures the post-login destination is a safe, same-site relative path. Any
/// value that is missing, not rooted at `/`, or that could be interpreted as a
/// protocol-relative or backslash-escaped URL falls back to the app root.
fn sanitize_return_to(value: Option<&str>) -> String {
    match value {
        Some(path)
            if path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\") =>
        {
            path.to_string()
        }
        _ => "/".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_return_to_accepts_local_paths() {
        assert_eq!(sanitize_return_to(Some("/queue")), "/queue");
        assert_eq!(sanitize_return_to(Some("/db?key=1")), "/db?key=1");
    }

    #[test]
    fn sanitize_return_to_rejects_external_destinations() {
        assert_eq!(sanitize_return_to(None), "/");
        assert_eq!(sanitize_return_to(Some("")), "/");
        assert_eq!(sanitize_return_to(Some("https://evil.example")), "/");
        assert_eq!(sanitize_return_to(Some("//evil.example")), "/");
        assert_eq!(sanitize_return_to(Some("/\\evil.example")), "/");
        assert_eq!(sanitize_return_to(Some("relative")), "/");
    }
}
