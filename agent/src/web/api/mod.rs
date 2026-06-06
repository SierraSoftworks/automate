//! The JSON REST API consumed by the single-page admin UI.
//!
//! All endpoints live under `/api/v1`. The `auth` sub-scope is public so that an
//! unauthenticated browser can drive the server-side OIDC login flow; every
//! other endpoint is gated by [`api_auth`], which authenticates the session
//! cookie (when OIDC is configured), evaluates the admin ACL, and enforces a
//! double-submit CSRF check on mutating requests.

use actix_web::{
    HttpResponse,
    body::BoxBody,
    cookie::Cookie,
    dev::{ServiceRequest, ServiceResponse},
    http::Method,
    middleware::{Next, from_fn},
    web,
};

use crate::prelude::*;
use crate::web::helpers::oidc::{
    AdminRequestFilter, admin_user_from_claims, filterable_claims, validate_token,
};

mod auth;
mod kv;
mod queue;
mod user;

/// The cookie holding the signed-in administrator's OIDC ID token (the session).
pub const SESSION_COOKIE: &str = "automate_session";

/// The non-`HttpOnly` cookie holding the double-submit CSRF token.
pub const CSRF_COOKIE: &str = "automate_csrf";

/// The short-lived cookie holding in-flight OAuth state during the login
/// redirect.
pub const OAUTH_COOKIE: &str = "automate_oauth";

/// The header the browser must echo the CSRF token back in on mutating requests.
const CSRF_HEADER: &str = "x-csrf-token";

/// The validated identity attached to a request after successful authentication,
/// made available to handlers via the request extensions.
#[derive(Clone)]
pub struct Authenticated {
    pub user: Option<automate_api::AdminUser>,
}

/// Builds a JSON error response with the given status code and message.
pub fn json_error(status: actix_web::http::StatusCode, message: impl ToString) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({ "error": message.to_string() }))
}

/// Registers the `/api/v1` routes. The `auth` endpoints are public; everything
/// else is wrapped in the [`api_auth`] middleware.
pub fn configure<S: Services + Clone + Send + Sync + 'static>() -> actix_web::Scope<
    impl actix_web::dev::ServiceFactory<
        ServiceRequest,
        Config = (),
        Response = ServiceResponse<BoxBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    web::scope("/api/v1")
        .service(
            web::scope("/auth")
                .route("/login", web::get().to(auth::auth_login::<S>))
                .route("/callback", web::get().to(auth::auth_callback::<S>))
                .route("/logout", web::post().to(auth::auth_logout)),
        )
        .service(
            web::scope("")
                .wrap(from_fn(api_auth::<S>))
                .route("/csrf", web::get().to(auth::csrf_token::<S>))
                .route("/me", web::get().to(user::me))
                .route("/kv", web::get().to(kv::list::<S>))
                .route("/kv/{partition}", web::delete().to(kv::delete::<S>))
                .route("/queue", web::get().to(queue::list::<S>))
                .route(
                    "/queue/{partition}/trigger",
                    web::post().to(queue::trigger::<S>),
                )
                .route("/queue/{partition}", web::delete().to(queue::delete::<S>)),
        )
}

/// Returns `true` for HTTP methods that mutate state and therefore require a
/// valid CSRF token.
fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Authentication and authorisation middleware for the protected API endpoints.
///
/// When OIDC is configured, a valid `automate_session` cookie (an ID token
/// issued by the server-side login flow) is required. The validated claims
/// (along with request metadata) are then evaluated against the admin ACL. When
/// OIDC is not configured, the ACL is evaluated against request metadata alone
/// (for example to restrict access by client IP).
///
/// Mutating requests (POST/PUT/PATCH/DELETE) must additionally present a
/// matching `X-CSRF-Token` header and `automate_csrf` cookie (the double-submit
/// pattern).
pub async fn api_auth<S: Services + Send + Sync + 'static>(
    req: ServiceRequest,
    next: Next<BoxBody>,
) -> Result<ServiceResponse<BoxBody>, actix_web::Error> {
    use actix_web::HttpMessage;

    let Some(services) = req.app_data::<web::Data<S>>().cloned() else {
        return Ok(req.into_response(json_error(
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Service context unavailable.",
        )));
    };

    let config = services.config();
    let admin = &config.web.admin;

    // Enforce the double-submit CSRF check before doing any other work on
    // mutating requests so a forged cross-site request is rejected early.
    if is_mutating(req.method()) && !csrf_ok(&req) {
        return Ok(req.into_response(json_error(
            actix_web::http::StatusCode::FORBIDDEN,
            "The request could not be verified. Please refresh the page and try again.",
        )));
    }

    // Authenticate via the session cookie when OIDC is configured.
    let claims = if let Some(oidc) = &admin.oidc {
        let token = req.cookie(SESSION_COOKIE).map(|c| c.value().to_string());

        let Some(token) = token else {
            return Ok(req.into_response(json_error(
                actix_web::http::StatusCode::UNAUTHORIZED,
                "Authentication is required to access this resource.",
            )));
        };

        match validate_token(services.as_ref(), oidc, &token).await {
            Ok(claims) => Some(claims),
            Err(e) => {
                info!("Rejected API request with an invalid session cookie: {e}");
                return Ok(req.into_response(json_error(
                    actix_web::http::StatusCode::UNAUTHORIZED,
                    "Your session is invalid or has expired. Please sign in again.",
                )));
            }
        }
    } else {
        None
    };

    let filterable = claims.as_ref().map(filterable_claims);
    let filter = AdminRequestFilter {
        method: req.method().as_str(),
        path: req.path(),
        client_ip: req.peer_addr().map(|addr| addr.ip().to_string()),
        headers: req.headers(),
        claims: filterable.as_ref(),
    };

    let allowed = admin.acl.matches(&filter).unwrap_or(false);

    if !allowed {
        let status = if claims.is_some() {
            actix_web::http::StatusCode::FORBIDDEN
        } else {
            actix_web::http::StatusCode::UNAUTHORIZED
        };
        return Ok(req.into_response(json_error(
            status,
            "Your account is not permitted to access this resource.",
        )));
    }

    let user = claims.as_ref().map(admin_user_from_claims);
    req.extensions_mut().insert(Authenticated { user });

    next.call(req).await
}

/// Validates the double-submit CSRF token: the `X-CSRF-Token` header must be
/// present, non-empty, and equal to the `automate_csrf` cookie value.
fn csrf_ok(req: &ServiceRequest) -> bool {
    let header = req
        .headers()
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let cookie = req
        .cookie(CSRF_COOKIE)
        .map(|c: Cookie| c.value().to_string());

    match (header, cookie) {
        (Some(header), Some(cookie)) => !header.is_empty() && header == cookie,
        _ => false,
    }
}
