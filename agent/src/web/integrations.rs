use std::collections::HashMap;

use actix_web::http::StatusCode;
use actix_web::{HttpRequest, HttpResponse, dev::HttpServiceFactory, web};

use crate::integrations::{Integration, IntegrationContext, Registry, state_cookie_path};
use crate::prelude::*;
use crate::services::{AppContext, AppServices};
use crate::web::api::Scoped;
use crate::web::helpers::request::{base_url, is_https};
use crate::web::helpers::wizard::{
    PublicWizardOutcome, SETUP_STATE_COOKIE, access_denied_page, admin_only_page, error_page,
    html_action_page, html_page, public_wizard_outcome, state_matches, with_cleared_state,
    wizard_state_cookie,
};

/// The setup routes are concrete over [`AppServices`] for the same reason
/// [`crate::job::JobRunnable`] is: the integration registry needs one concrete
/// services type to stay object-safe.
pub fn configure() -> impl HttpServiceFactory {
    web::scope("/integrations/{integration}/setup")
        .route("", web::get().to(setup_home))
        .route("/", web::get().to(setup_home))
        .route("/start", web::get().to(setup_start))
        .route("/callback", web::get().to(setup_callback))
}

/// OAuth2 providers keep their original callback path because the redirect URI
/// is registered with the provider, and cannot be moved without reconfiguring
/// every provider's application.
///
/// Mounted separately rather than folded into [`configure`] so an integration
/// only ever has the one callback route it actually uses. A second route it
/// never redirects to could not pass the state check anyway — the cookie is
/// scoped to the path the integration nominated — so it would be a permanently
/// failing endpoint.
pub fn configure_oauth_callback() -> impl HttpServiceFactory {
    web::resource("/oauth/{integration}/callback").route(web::get().to(setup_callback))
}

/// Translates a failure into a response, preserving the distinction the error
/// itself draws.
///
/// A user error is something the visitor or the operator can act on ("that
/// installation isn't ours", "no App is configured"), so its own message is
/// shown. A system error is our problem, and is reported generically after being
/// logged and recorded.
fn error_response(
    services: &AppServices,
    context: &str,
    err: human_errors::Error,
    render: impl FnOnce(u16, &str, &str) -> HttpResponse,
) -> HttpResponse {
    if err.is(human_errors::Kind::User) {
        warn!("{context}: {err}");
        render(400, "Bad Request", &err.description())
    } else {
        error!("{context}: {err}");
        services.session().record_human_error(&err);
        render(
            500,
            "Internal Server Error",
            "Something went wrong on our end. The failure has been recorded.",
        )
    }
}

fn html_error(status: u16, title: &str, message: &str) -> HttpResponse {
    error_page(status, title, message)
}

fn json_error(status: u16, _title: &str, message: &str) -> HttpResponse {
    crate::web::api::json_error(
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        message,
    )
}

/// Resolves the integration handling `id`, or renders `not_found`.
fn resolve<'a>(
    registry: &'a Registry,
    id: &str,
    not_found: impl FnOnce() -> HttpResponse,
) -> Result<(&'static dyn Integration, &'a str), HttpResponse> {
    match registry.get(id) {
        Some((integration, info)) => Ok((integration, info.name.as_str())),
        None => Err(not_found()),
    }
}

fn gate(
    services: &AppServices,
    req: &HttpRequest,
    integration: &dyn Integration,
    id: &str,
) -> Option<HttpResponse> {
    match public_wizard_outcome(
        services,
        req,
        integration.acl(&services.config(), id).as_ref(),
    ) {
        PublicWizardOutcome::Allowed => None,
        PublicWizardOutcome::Denied => Some(access_denied_page()),
        PublicWizardOutcome::AdminOnly => Some(admin_only_page()),
    }
}

async fn setup_home(
    services: web::Data<AppServices>,
    registry: web::Data<Registry>,
    integration: web::Path<String>,
    req: HttpRequest,
) -> HttpResponse {
    let services = services.get_ref();
    let id = integration.into_inner();

    let (integration, name) = match resolve(&registry, &id, || {
        html_error(
            404,
            "Not Found",
            "No such integration is configured on this Automate instance.",
        )
    }) {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    if let Some(denied) = gate(services, &req, integration, &id) {
        return denied;
    }

    html_action_page(
        &format!("{name} | Automate"),
        &format!("Connect {name}"),
        &format!("Click the button below to set up {name}."),
        &format!("/integrations/{id}/setup/start"),
        "Connect",
    )
}

#[instrument("web.integrations.start", skip(services, context, registry, req), fields(integration = %integration, otel.kind=?OpenTelemetrySpanKind::Server))]
async fn setup_start(
    services: web::Data<AppServices>,
    context: web::Data<AppContext>,
    registry: web::Data<Registry>,
    integration: web::Path<String>,
    req: HttpRequest,
) -> HttpResponse {
    let services = services.get_ref();
    let initiator = TenantId::local();
    let id = integration.into_inner();

    let (integration, _) = match resolve(&registry, &id, || {
        html_error(
            404,
            "Not Found",
            "No such integration is configured on this Automate instance.",
        )
    }) {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    if let Some(denied) = gate(services, &req, integration, &id) {
        return denied;
    }

    let Some(base) = base_url(services, req.headers(), req.uri().scheme_str()) else {
        return html_error(
            400,
            "Bad Request",
            "Your request did not include the required Host header.",
        );
    };

    match integration
        .begin_setup(
            &id,
            IntegrationContext {
                context: &context,
                initiator: initiator.clone(),
                base_url: &base,
            },
        )
        .await
    {
        Ok(redirect) => {
            let secure = is_https(
                services.config().web.trust_proxy,
                req.headers(),
                req.uri().scheme_str(),
            );

            HttpResponse::Found()
                .cookie(wizard_state_cookie(
                    &state_cookie_path(integration, &id),
                    redirect.state,
                    secure,
                ))
                .append_header((actix_web::http::header::LOCATION, redirect.url))
                .finish()
        }
        Err(err) => error_response(
            services,
            &format!("Failed to begin setup for '{id}'"),
            err,
            html_error,
        ),
    }
}

#[instrument("web.integrations.callback", skip(services, context, registry, req, query), fields(integration = %integration, otel.kind=?OpenTelemetrySpanKind::Server))]
async fn setup_callback(
    services: web::Data<AppServices>,
    context: web::Data<AppContext>,
    registry: web::Data<Registry>,
    integration: web::Path<String>,
    req: HttpRequest,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let services = services.get_ref();
    let initiator = TenantId::local();
    let id = integration.into_inner();

    let (integration, name) = match resolve(&registry, &id, || {
        html_error(
            404,
            "Not Found",
            "No such integration is configured on this Automate instance.",
        )
    }) {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let name = name.to_string();

    let cookie_path = state_cookie_path(integration, &id);

    // The `state` echoed back must match the value stored when the flow began,
    // otherwise someone could walk an admin through completing a flow of the
    // attacker's choosing.
    let expected_state = req
        .cookie(SETUP_STATE_COOKIE)
        .map(|c| c.value().to_string());
    if !state_matches(
        expected_state.as_deref(),
        query.get("state").map(String::as_str),
    ) {
        warn!("Rejected an integration setup callback with a missing or mismatched state.");
        return with_cleared_state(
            &cookie_path,
            html_error(
                400,
                "Bad Request",
                "The setup could not be verified. Please start again.",
            ),
        );
    }

    let Some(base) = base_url(services, req.headers(), req.uri().scheme_str()) else {
        return with_cleared_state(
            &cookie_path,
            html_error(
                400,
                "Bad Request",
                "Your request did not include the required Host header.",
            ),
        );
    };

    match integration
        .complete_setup(
            &id,
            IntegrationContext {
                context: &context,
                initiator: initiator.clone(),
                base_url: &base,
            },
            &query,
        )
        .await
    {
        Ok(outcome) => with_cleared_state(
            &cookie_path,
            html_page(
                200,
                &format!("{name} | Automate"),
                &outcome.heading,
                &outcome.message,
            ),
        ),
        Err(err) => with_cleared_state(
            &cookie_path,
            error_response(
                services,
                &format!("Failed to complete setup for '{id}'"),
                err,
                html_error,
            ),
        ),
    }
}

/// `GET /api/v1/integrations` — the integrations configured here, so the admin
/// SPA can offer a "connect" action for each. Admin-gated by `api_auth`.
pub async fn list(registry: web::Data<Registry>) -> HttpResponse {
    HttpResponse::Ok().json(registry.list())
}

/// `GET /api/v1/integrations/{integration}/connections` — the accounts currently
/// connected. Admin-gated by `api_auth`.
pub async fn list_connections(
    scoped: Scoped,
    context: web::Data<AppContext>,
    registry: web::Data<Registry>,
    integration: web::Path<String>,
    req: HttpRequest,
) -> HttpResponse {
    let services = &*scoped;
    let initiator = scoped.tenant().clone();
    let id = integration.into_inner();

    let (integration, _) = match resolve(&registry, &id, || {
        json_error(404, "Not Found", "No such integration is configured.")
    }) {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let Some(base) = base_url(services, req.headers(), req.uri().scheme_str()) else {
        return json_error(
            400,
            "Bad Request",
            "Could not determine the public base URL.",
        );
    };

    match integration
        .connections(
            &id,
            IntegrationContext {
                context: &context,
                initiator: initiator.clone(),
                base_url: &base,
            },
        )
        .await
    {
        Ok(connections) => HttpResponse::Ok().json(connections),
        Err(err) => error_response(
            services,
            &format!("Failed to list connections for '{id}'"),
            err,
            json_error,
        ),
    }
}

/// `DELETE /api/v1/integrations/{integration}/connections/{connection}` — severs
/// a connection. Admin-gated by `api_auth`.
#[instrument("web.integrations.disconnect", skip(scoped, context, registry, req), fields(otel.kind=?OpenTelemetrySpanKind::Server))]
pub async fn disconnect(
    scoped: Scoped,
    context: web::Data<AppContext>,
    registry: web::Data<Registry>,
    path: web::Path<(String, String)>,
    req: HttpRequest,
) -> HttpResponse {
    let services = &*scoped;
    let initiator = scoped.tenant().clone();
    let (id, connection) = path.into_inner();

    let (integration, _) = match resolve(&registry, &id, || {
        json_error(404, "Not Found", "No such integration is configured.")
    }) {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let Some(base) = base_url(services, req.headers(), req.uri().scheme_str()) else {
        return json_error(
            400,
            "Bad Request",
            "Could not determine the public base URL.",
        );
    };

    match integration
        .disconnect(
            &id,
            &connection,
            IntegrationContext {
                context: &context,
                initiator: initiator.clone(),
                base_url: &base,
            },
        )
        .await
    {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(err) => error_response(
            services,
            &format!("Failed to disconnect '{connection}' from '{id}'"),
            err,
            json_error,
        ),
    }
}

/// `POST /api/v1/integrations/{integration}/setup/start` — mints an
/// authorization URL for the admin SPA to open in a popup, and sets the state
/// cookie the callback verifies. Admin-gated by `api_auth`.
pub async fn start(
    scoped: Scoped,
    context: web::Data<AppContext>,
    registry: web::Data<Registry>,
    integration: web::Path<String>,
    req: HttpRequest,
) -> HttpResponse {
    let services = &*scoped;
    let initiator = scoped.tenant().clone();
    let id = integration.into_inner();

    let (integration, _) = match resolve(&registry, &id, || {
        json_error(404, "Not Found", "No such integration is configured.")
    }) {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let Some(base) = base_url(services, req.headers(), req.uri().scheme_str()) else {
        return json_error(
            400,
            "Bad Request",
            "Could not determine the public base URL for the setup redirect.",
        );
    };

    match integration
        .begin_setup(
            &id,
            IntegrationContext {
                context: &context,
                initiator: initiator.clone(),
                base_url: &base,
            },
        )
        .await
    {
        Ok(redirect) => {
            let secure = is_https(
                services.config().web.trust_proxy,
                req.headers(),
                req.uri().scheme_str(),
            );

            HttpResponse::Ok()
                .cookie(wizard_state_cookie(
                    &state_cookie_path(integration, &id),
                    redirect.state,
                    secure,
                ))
                .json(serde_json::json!({ "authorize_url": redirect.url }))
        }
        Err(err) => error_response(
            services,
            &format!("Failed to begin setup for '{id}'"),
            err,
            json_error,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as actix_test;
    use actix_web::{App, http::StatusCode};

    use crate::config::OidcConfig;
    use crate::filter::Filter;
    use crate::web::OAuth2Config;

    fn spotify(acl: Option<&str>) -> OAuth2Config {
        OAuth2Config {
            name: "Spotify".to_string(),
            deprecated_jobs: Vec::new(),
            acl: acl.map(|a| Filter::new(a).unwrap()),
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            auth_url: "https://accounts.spotify.com/authorize".to_string(),
            token_url: "https://accounts.spotify.com/api/token".to_string(),
            scopes: vec![],
            todoist: Default::default(),
        }
    }

    /// Services with a single OAuth2 provider configured, the given admin ACL, an
    /// optional provider ACL, and optionally admin OIDC (which the wizard gate
    /// only checks for presence, never contacts).
    async fn service(admin_acl: &str, provider_acl: Option<&str>, oidc: bool) -> AppContext {
        AppContext::new_mock(|config| {
            config.web.admin.acl = Filter::new(admin_acl).unwrap();
            // A fixed base URL so the wizard doesn't depend on a Host header.
            config.web.base_url = Some("http://localhost:8080".to_string());
            if oidc {
                config.web.admin.oidc = Some(OidcConfig {
                    endpoint: "https://auth.example.com".to_string(),
                    client_id: "client".to_string(),
                    client_secret: "secret".to_string(),
                    scopes: vec![],
                    username_claim: None,
                });
            }
            config
                .oauth2
                .insert("spotify".to_string(), spotify(provider_acl));
        })
        .await
        .unwrap()
    }

    macro_rules! app {
        ($context:expr) => {{
            let context = $context;
            let registry = Registry::new(&context.config()).unwrap();
            actix_test::init_service(
                App::new()
                    .app_data(web::Data::new(context.tenant(TenantId::local())))
                    .app_data(web::Data::new(context))
                    .app_data(web::Data::new(registry))
                    .service(configure())
                    .service(configure_oauth_callback()),
            )
            .await
        }};
    }

    async fn wizard_home_status(context: AppContext) -> StatusCode {
        let app = app!(context);
        let req = actix_test::TestRequest::get()
            .uri("/integrations/spotify/setup")
            .to_request();
        actix_test::call_service(&app, req).await.status()
    }

    #[actix_web::test]
    async fn the_wizard_requires_admin_auth_by_default() {
        // No integration ACL, deny-all admin ACL, OIDC off ⇒ access denied.
        assert_eq!(
            wizard_home_status(service("false", None, false).await).await,
            StatusCode::FORBIDDEN
        );
    }

    #[actix_web::test]
    async fn an_admin_gated_wizard_without_oidc_uses_the_admin_acl() {
        // With OIDC off the admin ACL is evaluated on request metadata, so a
        // permissive admin ACL still grants access via the public path.
        assert_eq!(
            wizard_home_status(service("true", None, false).await).await,
            StatusCode::OK
        );
    }

    #[actix_web::test]
    async fn an_admin_gated_wizard_with_oidc_is_admin_only() {
        // A top-level navigation can't carry the bearer, so the public path must
        // refuse regardless of how permissive the admin ACL is.
        assert_eq!(
            wizard_home_status(service("true", None, true).await).await,
            StatusCode::FORBIDDEN
        );
    }

    #[actix_web::test]
    async fn an_integration_acl_can_open_a_locked_down_instance() {
        assert_eq!(
            wizard_home_status(service("false", Some("true"), false).await).await,
            StatusCode::OK
        );
    }

    #[actix_web::test]
    async fn an_integration_acl_can_deny_despite_an_open_admin_acl() {
        assert_eq!(
            wizard_home_status(service("true", Some("false"), false).await).await,
            StatusCode::FORBIDDEN
        );
    }

    #[actix_web::test]
    async fn an_unknown_integration_is_not_found() {
        let app = app!(service("true", None, false).await);
        let req = actix_test::TestRequest::get()
            .uri("/integrations/nope/setup")
            .to_request();
        assert_eq!(
            actix_test::call_service(&app, req).await.status(),
            StatusCode::NOT_FOUND
        );
    }

    /// The callback is public — the provider redirects to it on a top-level
    /// navigation — and is protected solely by the transient state cookie. With
    /// no cookie the CSRF check must reject it before any code exchange.
    #[actix_web::test]
    async fn the_callback_rejects_a_missing_state() {
        let app = app!(service("true", None, false).await);
        let req = actix_test::TestRequest::get()
            .uri("/oauth/spotify/callback?code=abc&state=xyz")
            .to_request();
        assert_eq!(
            actix_test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[actix_web::test]
    async fn the_callback_rejects_a_mismatched_state() {
        let app = app!(service("true", None, false).await);
        let req = actix_test::TestRequest::get()
            .uri("/oauth/spotify/callback?code=abc&state=xyz")
            .cookie(
                actix_web::cookie::Cookie::build(SETUP_STATE_COOKIE, "a-different-state").finish(),
            )
            .to_request();
        assert_eq!(
            actix_test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    /// Starting the flow must hand the browser a state cookie scoped to the
    /// integration's own callback directory, or the callback can never verify it.
    #[actix_web::test]
    async fn starting_the_flow_sets_a_state_cookie_scoped_to_the_callback() {
        let app = app!(service("true", None, false).await);
        let req = actix_test::TestRequest::get()
            .uri("/integrations/spotify/setup/start")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::FOUND);

        let cookie = resp
            .response()
            .cookies()
            .find(|c| c.name() == SETUP_STATE_COOKIE)
            .expect("the flow should set a state cookie");
        assert_eq!(cookie.path(), Some("/oauth/spotify"));
        assert!(cookie.http_only().unwrap_or(false));
        assert!(!cookie.value().is_empty());

        let location = resp
            .headers()
            .get(actix_web::http::header::LOCATION)
            .and_then(|l| l.to_str().ok())
            .unwrap_or_default();
        assert!(
            location.starts_with("https://accounts.spotify.com/authorize"),
            "expected a redirect to the provider, got '{location}'"
        );
    }

    /// A user error names what the operator has to fix; only a system error is
    /// reported as an opaque 500. Asking for a provider that isn't configured is
    /// the closest reachable user error on this path.
    #[actix_web::test]
    async fn a_user_error_is_reported_as_a_bad_request() {
        let configured = service("true", None, false).await;
        let registry = Registry::new(&configured.config()).unwrap();

        // Resolve through the registry, then take the provider away so
        // `begin_setup` fails the way it would for a half-configured instance.
        let stripped = AppContext::new_mock(|config| {
            config.web.admin.acl = Filter::new("true").unwrap();
            config.web.base_url = Some("http://localhost:8080".to_string());
        })
        .await
        .unwrap();

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(stripped.tenant(TenantId::local())))
                .app_data(web::Data::new(stripped))
                .app_data(web::Data::new(registry))
                .service(configure()),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/integrations/spotify/setup/start")
            .to_request();
        assert_eq!(
            actix_test::call_service(&app, req).await.status(),
            StatusCode::BAD_REQUEST,
            "a misconfiguration is the operator's to fix, not an internal error"
        );
    }
}
