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
use crate::web::helpers::request::client_ip;

mod auth;
mod kv;
mod queue;
mod user;

/// The cookie holding the signed-in administrator's OIDC ID token (the session).
pub const SESSION_COOKIE: &str = "automate_session";

/// The `HttpOnly` cookie holding the OIDC refresh token, scoped to the auth
/// endpoints so it only travels with session-renewal (and logout) requests.
pub const REFRESH_COOKIE: &str = "automate_refresh";

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
                .route("/refresh", web::post().to(auth::auth_refresh::<S>))
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
        client_ip: client_ip(config.web.trust_proxy, req.headers(), req.peer_addr()),
        headers: req.headers(),
        claims: filterable.as_ref(),
    };

    let allowed = admin.acl.matches(&filter).unwrap_or(false);

    if !allowed {
        // We only reach the ACL check after authentication has already been
        // resolved: either OIDC is configured and the session validated above, or
        // OIDC is disabled and there is nothing to sign in to. In both cases a
        // denial here is a permanent authorization failure, so respond `403`.
        // Returning `401` would tell the browser to start a sign-in that cannot
        // change the outcome — and, when OIDC is disabled, would leave the admin
        // UI bouncing through a sign-in flow that goes nowhere.
        return Ok(req.into_response(json_error(
            actix_web::http::StatusCode::FORBIDDEN,
            "Your account is not permitted to access this resource.",
        )));
    }

    let user = claims.as_ref().map(admin_user_from_claims);
    req.extensions_mut().insert(Authenticated { user });

    next.call(req).await
}

/// Compares the two halves of a double-submit CSRF token: they must both be
/// present, non-empty, and equal.
fn csrf_tokens_match(header: Option<&str>, cookie: Option<&str>) -> bool {
    matches!((header, cookie), (Some(h), Some(c)) if !h.is_empty() && h == c)
}

/// Validates the double-submit CSRF token on a [`ServiceRequest`]: the
/// `X-CSRF-Token` header must equal the `automate_csrf` cookie. Used by the
/// [`api_auth`] middleware.
fn csrf_ok(req: &ServiceRequest) -> bool {
    let cookie = req.cookie(CSRF_COOKIE);
    csrf_tokens_match(
        req.headers().get(CSRF_HEADER).and_then(|v| v.to_str().ok()),
        cookie.as_ref().map(|c: &Cookie| c.value()),
    )
}

/// Validates the double-submit CSRF token on an [`actix_web::HttpRequest`]. Used
/// by the public logout handler, which sits outside the [`api_auth`] middleware
/// and so must perform the check itself.
pub(crate) fn csrf_ok_request(req: &actix_web::HttpRequest) -> bool {
    let cookie = req.cookie(CSRF_COOKIE);
    csrf_tokens_match(
        req.headers().get(CSRF_HEADER).and_then(|v| v.to_str().ok()),
        cookie.as_ref().map(|c: &Cookie| c.value()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::{App, cookie::Cookie as TestCookie, test, web};

    use crate::config::Config;
    use crate::db::SqliteDatabase;
    use crate::filter::Filter;
    use crate::services::ServicesContainer;

    /// Builds a services container with OIDC disabled and the given admin ACL.
    async fn service_with_acl(acl: &str) -> ServicesContainer<SqliteDatabase> {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let mut config = Config::default();
        config.web.admin.acl = Filter::new(acl).unwrap();
        ServicesContainer::new(config, db)
    }

    #[actix_web::test]
    async fn acl_denial_without_oidc_is_forbidden_not_unauthorized() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service_with_acl("false").await))
                .service(configure::<ServicesContainer<SqliteDatabase>>()),
        )
        .await;

        let req = test::TestRequest::get().uri("/api/v1/me").to_request();
        let resp = test::call_service(&app, req).await;

        // A denial while OIDC is disabled is permanent — there is nothing to sign
        // in to — so it must be a 403, never a 401 that would send the admin UI
        // into a sign-in flow that can never succeed.
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn acl_allow_without_oidc_reports_no_signed_in_user() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service_with_acl("true").await))
                .service(configure::<ServicesContainer<SqliteDatabase>>()),
        )
        .await;

        let req = test::TestRequest::get().uri("/api/v1/me").to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[actix_web::test]
    async fn mutating_request_without_csrf_is_rejected() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service_with_acl("true").await))
                .service(configure::<ServicesContainer<SqliteDatabase>>()),
        )
        .await;

        let req = test::TestRequest::delete()
            .uri("/api/v1/kv/cache?key=foo")
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn logout_requires_a_matching_csrf_token() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(service_with_acl("true").await))
                .service(configure::<ServicesContainer<SqliteDatabase>>()),
        )
        .await;

        // No CSRF token: rejected, so a cross-site POST cannot force a logout.
        let req = test::TestRequest::post()
            .uri("/api/v1/auth/logout")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // A matching double-submit header and cookie: accepted.
        let req = test::TestRequest::post()
            .uri("/api/v1/auth/logout")
            .cookie(TestCookie::new(CSRF_COOKIE, "tok"))
            .insert_header((CSRF_HEADER, "tok"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }
}
