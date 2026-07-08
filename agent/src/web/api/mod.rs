//! The JSON REST API consumed by the single-page admin UI.
//!
//! All endpoints live under `/api/v1`. The `auth` sub-scope is public so that an
//! unauthenticated browser can run the OIDC popup login (fetch metadata, exchange
//! a code, refresh a session); every other endpoint is gated by [`api_auth`],
//! which authenticates the `Authorization: Bearer` ID token (when OIDC is
//! configured) and evaluates the admin ACL. Because the credential is a bearer
//! header — never an automatically-attached cookie — there is no CSRF surface and
//! no double-submit token to verify.

use actix_web::{
    HttpResponse,
    body::BoxBody,
    dev::{ServiceRequest, ServiceResponse},
    http::header::AUTHORIZATION,
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

/// The validated identity attached to a request after successful authentication,
/// made available to handlers via the request extensions.
#[derive(Clone)]
pub struct Authenticated {
    pub user: Option<automate_api::AdminUser>,
}

/// Extracts a bearer token from the `Authorization` header, accepting either
/// capitalisation of the `Bearer` scheme.
pub(crate) fn bearer_token(headers: &actix_web::http::header::HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(|token| token.trim().to_string())
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
                .route("/metadata", web::get().to(auth::metadata::<S>))
                .route("/token", web::post().to(auth::auth_token::<S>))
                .route("/refresh", web::post().to(auth::auth_refresh::<S>)),
        )
        .service(
            web::scope("")
                .wrap(from_fn(api_auth::<S>))
                .route("/me", web::get().to(user::me))
                .route("/kv", web::get().to(kv::list::<S>))
                .route("/kv/{partition}", web::delete().to(kv::delete::<S>))
                .route("/queue", web::get().to(queue::list::<S>))
                .route(
                    "/queue/{partition}/trigger",
                    web::post().to(queue::trigger::<S>),
                )
                .route("/queue/{partition}", web::delete().to(queue::delete::<S>))
                // The setup wizard is launched from the admin SPA: list the
                // configured providers and mint a popup authorization URL. Both
                // are admin-gated by `api_auth`.
                .route(
                    "/oauth",
                    web::get().to(crate::web::oauth::list_providers::<S>),
                )
                .route(
                    "/oauth/{provider}/start",
                    web::post().to(crate::web::oauth::start::<S>),
                ),
        )
}

/// Authentication and authorisation middleware for the protected API endpoints.
///
/// When OIDC is configured, a valid `Authorization: Bearer` ID token (issued by
/// the popup login flow) is required. The validated claims (along with request
/// metadata) are then evaluated against the admin ACL. When OIDC is not
/// configured, the ACL is evaluated against request metadata alone (for example
/// to restrict access by client IP).
///
/// Because the credential is a bearer header rather than an automatically
/// attached cookie, a cross-site page cannot forge an authenticated request, so
/// no CSRF defence is required.
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

    // Authenticate via the bearer token when OIDC is configured.
    let claims = if let Some(oidc) = &admin.oidc {
        let Some(token) = bearer_token(req.headers()) else {
            return Ok(req.into_response(json_error(
                actix_web::http::StatusCode::UNAUTHORIZED,
                "Authentication is required to access this resource.",
            )));
        };

        match validate_token(services.as_ref(), oidc, &token).await {
            Ok(claims) => Some(claims),
            Err(e) => {
                info!("Rejected API request with an invalid bearer token: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::{App, test, web};

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
    async fn mutating_request_without_oidc_is_allowed_by_a_permissive_acl() {
        // With OIDC disabled there is no bearer to present; access is governed by
        // the ACL alone, and a bearer-only model has no CSRF token to reject a
        // mutating request on.
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

        // Reaches the handler (the ACL allows it); the key simply doesn't exist.
        assert_ne!(resp.status(), StatusCode::FORBIDDEN);
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn bearer_extraction_accepts_either_capitalisation() {
        use actix_web::http::header::{HeaderMap, HeaderName, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer abc.def.ghi"),
        );
        assert_eq!(bearer_token(&headers).as_deref(), Some("abc.def.ghi"));

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("bearer abc.def.ghi"),
        );
        assert_eq!(bearer_token(&headers).as_deref(), Some("abc.def.ghi"));

        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }
}
