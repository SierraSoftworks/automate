//! OpenID Connect (OIDC) authentication endpoints for the admin scope.
//!
//! When `[web.admin.oidc]` is configured, every request to the `/admin` scope
//! must carry a session cookie holding a valid ID token issued by the
//! configured provider. Requests without a valid session are redirected to the
//! provider's authorization endpoint to sign in. Once authenticated, the
//! validated token claims are exposed to the admin ACL filter under the
//! `claims.` prefix (for example `claims.email endswith "@example.com"`).
//!
//! This module wires up the callback endpoint and the access-control
//! middleware; the underlying OIDC machinery lives in
//! [`crate::web::helpers::oidc`].

use actix_web::{
    HttpMessage, HttpRequest, HttpResponse,
    body::BoxBody,
    dev::{ServiceRequest, ServiceResponse},
    http::header,
    middleware::Next,
    web,
};

use crate::prelude::*;
use crate::web::helpers::oidc::{
    AdminRequestFilter, AdminUser, CALLBACK_PATH_SUFFIX, LOGIN_COOKIE, LoginTransaction,
    SESSION_COOKIE, begin_login, build_cookie, clear_cookie, discovery, exchange_code,
    filterable_claims, redirect_base_url, safe_return_to, session_max_age, validate_token,
};
use crate::web::helpers::request::is_https;
use crate::web::ui::error_page;

/// Query parameters returned by the provider on the callback endpoint.
#[derive(serde::Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Handles the OIDC redirect callback: validates the transaction, exchanges the
/// authorization code for an ID token, verifies it, and establishes the admin
/// session cookie.
#[instrument("web.oidc.callback", skip(services, req, query), err(Display))]
pub async fn oidc_callback<S: Services + Send + Sync + 'static>(
    services: web::Data<S>,
    req: HttpRequest,
    query: web::Query<CallbackQuery>,
) -> Result<HttpResponse, actix_web::Error> {
    let Some(oidc) = services.config().web.admin.oidc.clone() else {
        return Ok(not_configured().await);
    };

    if let Some(error) = &query.error {
        warn!(
            "OIDC provider returned an error on callback: {error} ({})",
            query
                .error_description
                .as_deref()
                .unwrap_or("no description")
        );
        return Ok(error_page(
            400,
            "Sign-in Failed",
            "The identity provider reported an error while signing you in.",
        )
        .await);
    }

    let headers = req.headers();
    let uri_scheme = req.uri().scheme_str();
    let secure = is_https(services.config().web.trust_proxy, headers, uri_scheme);

    let transaction = match req.cookie(LOGIN_COOKIE) {
        Some(cookie) => match serde_json::from_str::<LoginTransaction>(cookie.value()) {
            Ok(tx) => tx,
            Err(_) => {
                return Ok(error_page(
                    400,
                    "Sign-in Failed",
                    "Your login session was invalid. Please try signing in again.",
                )
                .await);
            }
        },
        None => {
            return Ok(error_page(
                400,
                "Sign-in Failed",
                "Your login session has expired. Please try signing in again.",
            )
            .await);
        }
    };

    let (Some(code), Some(state)) = (query.code.as_deref(), query.state.as_deref()) else {
        return Ok(error_page(
            400,
            "Sign-in Failed",
            "The identity provider's response was missing required parameters.",
        )
        .await);
    };

    // Constant-time-ish comparison is unnecessary here because the state is a
    // random, single-use value; a direct comparison is sufficient to thwart
    // CSRF.
    if state != transaction.state {
        return Ok(error_page(
            400,
            "Sign-in Failed",
            "The login request could not be verified. Please try signing in again.",
        )
        .await);
    }

    let Some(base) = redirect_base_url(services.as_ref(), &oidc, headers, uri_scheme) else {
        return Ok(error_page(
            400,
            "Bad Request",
            "Your request did not include a Host header.",
        )
        .await);
    };

    let discovery = match discovery(services.as_ref(), &oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document during callback: {e}");
            return Ok(error_page(
                502,
                "Bad Gateway",
                "We could not reach the configured identity provider.",
            )
            .await);
        }
    };

    let redirect_uri = format!("{base}/admin{CALLBACK_PATH_SUFFIX}");
    let token = match exchange_code(&oidc, &discovery, code, &redirect_uri, &services.http_client()).await {
        Ok(token) => token,
        Err(e) => {
            error!("OIDC token exchange failed: {e}");
            return Ok(error_page(
                502,
                "Sign-in Failed",
                "We could not complete the sign-in with the identity provider.",
            )
            .await);
        }
    };

    let claims = match validate_token(services.as_ref(), &oidc, &token).await {
        Ok(claims) => claims,
        Err(e) => {
            warn!("OIDC ID token validation failed during callback: {e}");
            return Ok(error_page(
                400,
                "Sign-in Failed",
                "The identity provider issued a token we could not verify.",
            )
            .await);
        }
    };

    // Bind the token to this login attempt by checking the nonce.
    let nonce_ok = claims
        .get("nonce")
        .and_then(|v| v.as_str())
        .map(|n| n == transaction.nonce)
        .unwrap_or(false);
    if !nonce_ok {
        warn!("OIDC ID token nonce did not match the login transaction");
        return Ok(error_page(
            400,
            "Sign-in Failed",
            "The login request could not be verified. Please try signing in again.",
        )
        .await);
    }

    let max_age = session_max_age(&claims);
    let session_cookie = build_cookie(SESSION_COOKIE, token, secure, max_age);
    let cleared_login = clear_cookie(LOGIN_COOKIE, secure);

    Ok(HttpResponse::Found()
        .cookie(session_cookie)
        .cookie(cleared_login)
        .insert_header((header::LOCATION, transaction.return_to))
        .finish())
}

/// Renders the response used when OIDC is required but unavailable/misconfigured.
async fn not_configured() -> HttpResponse {
    error_page(
        404,
        "Not Found",
        "The page you are looking for does not exist.",
    )
    .await
}

/// Middleware enforcing admin access control (and OIDC authentication when
/// configured) across the `/admin` scope.
pub async fn admin_auth<S: Services + Send + Sync + 'static>(
    req: ServiceRequest,
    next: Next<BoxBody>,
) -> Result<ServiceResponse<BoxBody>, actix_web::Error> {
    // The OIDC callback authenticates itself; let it through without requiring
    // an existing session.
    if req.path().ends_with(CALLBACK_PATH_SUFFIX) {
        return next.call(req).await;
    }

    let Some(services) = req.app_data::<web::Data<S>>().cloned() else {
        // Without access to our services we cannot evaluate the ACL, so fail
        // closed.
        let response = error_page(500, "Internal Server Error", "Service context unavailable.")
            .await
            .map_into_boxed_body();
        return Ok(req.into_response(response));
    };

    let config = services.config();
    let admin = &config.web.admin;
    let uri_scheme = req.uri().scheme_str().map(|s| s.to_string());

    // Authenticate via OIDC when configured.
    let claims = if let Some(oidc) = &admin.oidc {
        match req.cookie(SESSION_COOKIE) {
            Some(cookie) => match validate_token(services.as_ref(), oidc, cookie.value()).await {
                Ok(claims) => Some(claims),
                Err(e) => {
                    info!("Admin session token rejected, redirecting to sign-in: {e}");
                    let return_to = safe_return_to(
                        req.uri()
                            .path_and_query()
                            .map(|pq| pq.as_str())
                            .unwrap_or("/admin"),
                    );
                    let response = begin_login(
                        services.as_ref(),
                        oidc,
                        req.headers(),
                        uri_scheme.as_deref(),
                        &return_to,
                    )
                    .await
                    .map_into_boxed_body();
                    return Ok(req.into_response(response));
                }
            },
            None => {
                let return_to = safe_return_to(
                    req.uri()
                        .path_and_query()
                        .map(|pq| pq.as_str())
                        .unwrap_or("/admin"),
                );
                let response = begin_login(
                    services.as_ref(),
                    oidc,
                    req.headers(),
                    uri_scheme.as_deref(),
                    &return_to,
                )
                .await
                .map_into_boxed_body();
                return Ok(req.into_response(response));
            }
        }
    } else {
        None
    };

    let filterable_claims = claims.as_ref().map(filterable_claims);
    let filter = AdminRequestFilter {
        method: req.method().as_str(),
        path: req.path(),
        client_ip: req.peer_addr().map(|addr| addr.ip().to_string()),
        headers: req.headers(),
        claims: filterable_claims.as_ref(),
    };

    let allowed = admin.acl.matches(&filter).unwrap_or(false);

    // Expose the signed-in user's identity to the admin UI for display.
    if let Some(claims) = &claims {
        req.extensions_mut().insert(AdminUser::from_claims(claims));
    }

    if allowed {
        return next.call(req).await;
    }

    // When the user has authenticated but is not authorised, tell them so;
    // otherwise preserve the existing behaviour of hiding the admin area
    // entirely behind a 404.
    let response = if claims.is_some() {
        error_page(
            403,
            "Forbidden",
            "Your account is not permitted to access the admin area.",
        )
        .await
    } else {
        error_page(
            404,
            "Not Found",
            "The page you are looking for does not exist.",
        )
        .await
    }
    .map_into_boxed_body();

    Ok(req.into_response(response))
}
