use crate::prelude::*;
use actix_web::body::BoxBody;
use actix_web::cookie::time::Duration as CookieDuration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::{Next, from_fn};
use actix_web::{HttpRequest, HttpResponse, dev::HttpServiceFactory, web};
use oauth2::{CsrfToken, Scope, TokenResponse};
use reqwest::Url;
use serde::Deserialize;

use crate::prelude::Services;
use crate::web::helpers::oidc::{AdminRequestFilter, filterable_claims, validate_token};
use crate::web::helpers::request::client_ip;

/// The transient cookie that carries the OAuth setup wizard's CSRF `state` across
/// the redirect to the provider, so the callback can confirm the response
/// belongs to a flow this browser actually started.
const OAUTH_SETUP_STATE_COOKIE: &str = "automate_oauth_setup";

/// How long the OAuth setup wizard's state cookie remains valid.
const OAUTH_SETUP_STATE_SECONDS: i64 = 10 * 60;

/// Renders a minimal, self-contained HTML page for the server-side OAuth setup
/// wizard. All interpolated values are HTML-escaped to avoid injection.
fn html_page(status: u16, title: &str, heading: &str, message: &str) -> HttpResponse {
    let body = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"/>\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\
<title>{title}</title></head><body style=\"font-family: system-ui, sans-serif; max-width: 40rem; margin: 4rem auto; padding: 0 1rem;\">\
<h1>{heading}</h1><p>{message}</p></body></html>",
        title = html_escape::encode_text(title),
        heading = html_escape::encode_text(heading),
        message = html_escape::encode_text(message),
    );

    HttpResponse::build(actix_web::http::StatusCode::from_u16(status).unwrap())
        .content_type("text/html; charset=utf-8")
        .body(body)
}

/// Renders a page with a call-to-action link (used to start the login flow).
fn html_action_page(
    title: &str,
    heading: &str,
    message: &str,
    href: &str,
    label: &str,
) -> HttpResponse {
    let body = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"/>\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\
<title>{title}</title></head><body style=\"font-family: system-ui, sans-serif; max-width: 40rem; margin: 4rem auto; padding: 0 1rem;\">\
<h1>{heading}</h1><p>{message}</p><a href=\"{href}\"><button>{label}</button></a></body></html>",
        title = html_escape::encode_text(title),
        heading = html_escape::encode_text(heading),
        message = html_escape::encode_text(message),
        href = html_escape::encode_double_quoted_attribute(href),
        label = html_escape::encode_text(label),
    );

    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(body)
}

fn error_page(status: u16, title: &str, message: &str) -> HttpResponse {
    html_page(status, title, title, message)
}

fn not_found() -> HttpResponse {
    error_page(
        404,
        "Not Found",
        "The requested resource could not be found.",
    )
}

/// Builds the transient state cookie for the OAuth setup wizard. It is scoped to
/// the provider's own `/oauth/{provider}` path and `SameSite=Lax` so it is
/// returned on the provider's top-level redirect back to the callback.
fn oauth_state_cookie(provider: &str, state: String, secure: bool) -> Cookie<'static> {
    Cookie::build(OAUTH_SETUP_STATE_COOKIE, state)
        .path(format!("/oauth/{provider}"))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(CookieDuration::seconds(OAUTH_SETUP_STATE_SECONDS))
        .finish()
}

/// Builds a removal for the OAuth setup wizard's state cookie (one-shot: it is
/// cleared as soon as the callback resolves, whatever the outcome).
fn clear_oauth_state_cookie(provider: &str) -> Cookie<'static> {
    let mut removal = Cookie::build(OAUTH_SETUP_STATE_COOKIE, "")
        .path(format!("/oauth/{provider}"))
        .finish();
    removal.make_removal();
    removal
}

/// Attaches the state-cookie removal to a callback response.
fn with_cleared_state(provider: &str, mut response: HttpResponse) -> HttpResponse {
    let _ = response.add_cookie(&clear_oauth_state_cookie(provider));
    response
}

/// Validates the OAuth `state`: the value echoed back by the provider must equal
/// the (non-empty) value stored in the browser's state cookie.
fn state_matches(expected: Option<&str>, provided: Option<&str>) -> bool {
    matches!((expected, provided), (Some(a), Some(b)) if !a.is_empty() && a == b)
}

/// The outcome of evaluating wizard access for a request.
enum WizardOutcome {
    /// The visitor may proceed.
    Authorized,
    /// The visitor isn't signed in and signing in could grant access.
    NeedsLogin,
    /// The visitor is denied and signing in would not help.
    Forbidden,
}

/// Determines whether a request may use a provider's setup wizard.
///
/// A provider that defines its own `acl` opts into self-service access: the ACL
/// is evaluated directly (a session is consulted when present but never required,
/// so `acl = 'true'` lets anyone connect). A provider without an `acl` is
/// admin-gated and behaves exactly like the admin API — when OIDC is configured a
/// valid session is required before the admin ACL is consulted.
async fn authorize_wizard<S: Services>(
    services: &S,
    req: &HttpRequest,
    provider_config: Option<&OAuth2Config>,
) -> WizardOutcome {
    let config = services.config();
    let admin = &config.web.admin;

    let (acl, require_session) = match provider_config.and_then(|c| c.acl.as_ref()) {
        Some(acl) => (acl, false),
        None => (&admin.acl, true),
    };

    // Validate the session cookie when OIDC is configured and one is present; an
    // absent or invalid cookie simply means "not signed in".
    let claims = match (admin.oidc.as_ref(), req.cookie(super::api::SESSION_COOKIE)) {
        (Some(oidc), Some(cookie)) => validate_token(services, oidc, cookie.value()).await.ok(),
        _ => None,
    };

    // Admin-gated wizards require a valid session before the ACL is consulted,
    // exactly as the admin API does.
    if require_session && admin.oidc.is_some() && claims.is_none() {
        return WizardOutcome::NeedsLogin;
    }

    let filterable = claims.as_ref().map(filterable_claims);
    let filter = AdminRequestFilter {
        method: req.method().as_str(),
        path: req.path(),
        client_ip: client_ip(config.web.trust_proxy, req.headers(), req.peer_addr()),
        headers: req.headers(),
        claims: filterable.as_ref(),
    };

    if acl.matches(&filter).unwrap_or(false) {
        WizardOutcome::Authorized
    } else if admin.oidc.is_some() && claims.is_none() {
        // Not signed in, and signing in might supply claims that satisfy the ACL.
        WizardOutcome::NeedsLogin
    } else {
        WizardOutcome::Forbidden
    }
}

/// An HTML interstitial prompting the visitor to sign in, returning to the
/// current wizard URL afterwards. Only shown when OIDC is configured.
fn sign_in_page(req: &HttpRequest) -> HttpResponse {
    let return_to = req.uri().to_string();
    let login = format!(
        "/api/v1/auth/login?return_to={}",
        urlencoding::encode(&return_to)
    );
    html_action_page(
        "Sign in | Automate",
        "Sign in required",
        "You need to sign in before you can set up this integration.",
        &login,
        "Sign in",
    )
}

/// An HTML page shown when the visitor is authenticated (or no sign-in is
/// possible) but not permitted to use the wizard.
fn access_denied_page() -> HttpResponse {
    error_page(
        403,
        "Access denied",
        "Your account is not permitted to set up this integration.",
    )
}

/// Authentication/authorization gate for the OAuth setup wizard.
///
/// Unlike the JSON admin API this renders HTML: an unauthenticated visitor is
/// offered a sign-in link (rather than a bare `401`), and a denied visitor gets a
/// readable page. Per-provider `acl`s are honoured via [`authorize_wizard`].
async fn oauth_wizard_auth<S: Services + Send + Sync + 'static>(
    req: ServiceRequest,
    next: Next<BoxBody>,
) -> Result<ServiceResponse<BoxBody>, actix_web::Error> {
    let Some(services) = req.app_data::<web::Data<S>>().cloned() else {
        return Ok(req.into_response(error_page(
            500,
            "Internal Server Error",
            "Service context unavailable.",
        )));
    };

    // The scope is `/oauth/{provider}`; read the provider straight from the path
    // so we don't depend on route-level match extraction in the middleware.
    let provider = req
        .path()
        .strip_prefix("/oauth/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("");
    let provider_config = services.config().oauth2.get(provider).cloned();

    match authorize_wizard(services.as_ref(), req.request(), provider_config.as_ref()).await {
        WizardOutcome::Authorized => next.call(req).await,
        WizardOutcome::NeedsLogin => {
            let response = sign_in_page(req.request());
            Ok(req.into_response(response))
        }
        WizardOutcome::Forbidden => Ok(req.into_response(access_denied_page())),
    }
}

pub fn configure<S: Services + Send + Sync + 'static>() -> impl HttpServiceFactory {
    web::scope("/oauth/{provider}")
        // The setup wizard performs privileged actions (linking external accounts
        // the agent then acts on), so gate it behind an authentication/ACL check.
        // By default that is the admin gate, but a provider may set its own `acl`
        // to allow self-service sign-up. The flow is browser-driven via top-level
        // GET navigations, so any session cookie is carried even on the provider's
        // cross-site redirect back to `/callback`.
        .wrap(from_fn(oauth_wizard_auth::<S>))
        .route("/", web::get().to(oauth_home::<S>))
        .route("/authorize", web::get().to(oauth_authorize::<S>))
        .route("/callback", web::get().to(oauth_callback::<S>))
}

async fn oauth_home<S: Services + Send + Sync + 'static>(
    provider: web::Path<String>,
    services: web::Data<S>,
) -> actix_web::HttpResponse {
    if let Some(config) = services.config().oauth2.get(&*provider).cloned() {
        html_action_page(
            &format!("{} | Automate", config.name),
            &format!("Login with {}", config.name),
            &format!(
                "Click the button below to initiate the setup process for {}.",
                config.name
            ),
            &format!("/oauth/{}/authorize", &*provider),
            "Login",
        )
    } else {
        not_found()
    }
}

#[instrument(
    "web.oauth.authorize",
    skip(provider, services, req),
    fields(oauth.provider = %provider, otel.kind=?OpenTelemetrySpanKind::Server),
)]
async fn oauth_authorize<S: Services + Send + Sync + 'static>(
    provider: web::Path<String>,
    services: web::Data<S>,
    req: actix_web::HttpRequest,
) -> impl actix_web::Responder {
    if let Some(base_url) =
        super::helpers::request::base_url(services.as_ref(), req.headers(), req.uri().scheme_str())
    {
        match services.config().oauth2.get(&*provider).cloned() {
            Some(cfg) => {
                info!("Initiating OAuth2 login flow for provider '{}'", &*provider);

                match cfg.get_login_url(format!("{base_url}/oauth/{provider}/callback")) {
                    Ok((url, state)) => {
                        // Persist the CSRF `state` in the browser so the callback
                        // can verify it; mark the cookie `Secure` over HTTPS.
                        let secure = super::helpers::request::is_https(
                            services.config().web.trust_proxy,
                            req.headers(),
                            req.uri().scheme_str(),
                        );
                        actix_web::HttpResponse::Found()
                            .cookie(oauth_state_cookie(&provider, state, secure))
                            .append_header((actix_web::http::header::LOCATION, url.to_string()))
                            .finish()
                    }
                    Err(e) => {
                        error!("Failed to get OAuth login URL: {}", e);
                        sentry::capture_error(&e);
                        error_page(
                            500,
                            "Internal Server Error",
                            "Failed to initiate OAuth login process.",
                        )
                    }
                }
            }
            None => {
                warn!(
                    "OAuth provider '{}' not found in configuration.",
                    &*provider
                );
                not_found()
            }
        }
    } else {
        error_page(
            400,
            "Bad Request",
            "Your request did not include the required Host header.",
        )
    }
}

#[instrument(
    "web.oauth.callback",
    skip(provider, query, services, req),
    fields(oauth.provider = %provider, otel.kind=?OpenTelemetrySpanKind::Server),
)]
async fn oauth_callback<S: Services + Send + Sync + 'static>(
    services: web::Data<S>,
    provider: web::Path<String>,
    req: actix_web::HttpRequest,
    query: web::Query<std::collections::HashMap<String, String>>,
) -> actix_web::HttpResponse {
    let Some(base_url) =
        super::helpers::request::base_url(services.as_ref(), req.headers(), req.uri().scheme_str())
    else {
        return error_page(
            400,
            "Bad Request",
            "Your request did not include the required Host header.",
        );
    };

    let Some(config) = services.config().oauth2.get(&*provider).cloned() else {
        return error_page(400, "Bad Request", "Invalid OAuth provider specified.");
    };

    // CSRF: the `state` echoed back by the provider must match the value we stored
    // in the browser's state cookie when the flow began. Without this an attacker
    // could trick a signed-in admin into completing the flow with an authorization
    // code of the attacker's choosing, linking the attacker's account to the agent.
    let expected_state = req
        .cookie(OAUTH_SETUP_STATE_COOKIE)
        .map(|c| c.value().to_string());
    if !state_matches(
        expected_state.as_deref(),
        query.get("state").map(String::as_str),
    ) {
        warn!("Rejected an OAuth setup callback with a missing or mismatched state.");
        return with_cleared_state(
            &provider,
            error_page(
                400,
                "Bad Request",
                "The login could not be verified. Please start the setup again.",
            ),
        );
    }

    let Some(code) = query.get("code") else {
        return with_cleared_state(
            &provider,
            error_page(
                400,
                "Bad Request",
                "Missing 'code' parameter in OAuth callback.",
            ),
        );
    };

    match config
        .handle_callback(
            format!("{base_url}/oauth/{provider}/callback"),
            code.clone(),
            &services.http_client(),
        )
        .await
    {
        Ok(token) => {
            let partitions = config.jobs.clone();
            for partition in partitions.into_iter() {
                if let Err(e) = services
                    .queue()
                    .enqueue(partition, token.clone(), None, None)
                    .await
                {
                    error!("Failed to enqueue OAuth token storage task: {}", e);
                    return with_cleared_state(
                        &provider,
                        error_page(
                            500,
                            "Internal Server Error",
                            "Failed to store OAuth token, please try again later.",
                        ),
                    );
                }
            }

            with_cleared_state(
                &provider,
                html_page(
                    200,
                    &format!("{} | Automate", config.name),
                    "Login Complete",
                    &format!(
                        "You have successfully completed setting up {}, you can close this window.",
                        config.name
                    ),
                ),
            )
        }
        Err(e) => {
            error!("OAuth callback handling failed: {}", e);
            sentry::capture_error(&e);
            with_cleared_state(
                &provider,
                error_page(
                    500,
                    "Internal Server Error",
                    "Failed to complete OAuth token exchange.",
                ),
            )
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct OAuth2Config {
    pub name: String,

    #[serde(default)]
    pub jobs: Vec<String>,

    /// Optional access-control filter governing who may use this provider's setup
    /// wizard. It is evaluated exactly like the admin ACL — against the request
    /// `method`, `path`, `client_ip`, `headers.*`, and (when OIDC is configured
    /// and the visitor is signed in) `claims.*`. When omitted the wizard is
    /// admin-gated: it falls back to the admin ACL and requires admin sign-in,
    /// just like the rest of the admin area. Set `acl = 'true'` to let anyone
    /// connect this provider without signing in.
    #[serde(default)]
    pub acl: Option<Filter>,

    pub client_id: String,
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl OAuth2Config {
    pub fn get_login_url(
        &self,
        redirect_url: impl ToString,
    ) -> Result<(Url, String), human_errors::Error> {
        let client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(self.client_id.clone()))
            .set_client_secret(oauth2::ClientSecret::new(self.client_secret.clone()))
            .set_auth_uri(oauth2::AuthUrl::new(self.auth_url.clone()).or_user_err(&[
                "Ensure that you have provided a valid `oauth2.xxx.auth_url` in your configuration file.",
            ])?)
            .set_token_uri(oauth2::TokenUrl::new(self.token_url.clone()).or_user_err(&[
                "Ensure that you have provided a valid `oauth2.xxx.token_url` in your configuration file.",
            ])?)
            .set_redirect_uri(
                oauth2::RedirectUrl::new(redirect_url.to_string()).or_system_err(&[
                    "Ensure that your proxy is sending the x-forwarded-host and x-forwarded-proto headers correctly.",
                ])?,
            );

        let (url, csrf) = client
            .authorize_url(CsrfToken::new_random)
            .add_scopes(self.scopes.iter().cloned().map(Scope::new))
            .url();
        Ok((url, csrf.secret().clone()))
    }

    pub async fn handle_callback(
        &self,
        redirect_url: impl ToString,
        code: impl ToString,
        http_client: &reqwest::Client,
    ) -> Result<OAuth2RefreshToken, human_errors::Error> {
        let client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(self.client_id.clone()))
            .set_client_secret(oauth2::ClientSecret::new(self.client_secret.clone()))
            .set_auth_uri(oauth2::AuthUrl::new(self.auth_url.clone()).or_system_err(&[])?)
            .set_token_uri(oauth2::TokenUrl::new(self.token_url.clone()).or_system_err(&[])?)
            .set_redirect_uri(
                oauth2::RedirectUrl::new(redirect_url.to_string()).or_system_err(&[
                    "Ensure that your proxy is sending the x-forwarded-host and x-forwarded-proto headers correctly.",
                ])?,
            );

        let token_result = client
            .exchange_code(oauth2::AuthorizationCode::new(code.to_string()))
            .request_async(http_client)
            .await
            .wrap_user_err(
                format!("Failed to obtain OAuth access token for {}.", &self.name),
                &[
                    "Ensure that your OAuth client credentials are correct.",
                    "Check your network connection.",
                ],
            )?;

        Ok(OAuth2RefreshToken {
            access_token: token_result.access_token().secret().to_string(),
            refresh_token: token_result
                .refresh_token()
                .map(|t| t.secret().to_string())
                .unwrap_or_default(),
            expires_at: chrono::Utc::now()
                + chrono::Duration::seconds(
                    token_result
                        .expires_in()
                        .unwrap_or(std::time::Duration::from_secs(3600))
                        .as_secs() as i64,
                ),
        })
    }

    pub async fn get_access_token(
        &self,
        token_entry: &OAuth2RefreshToken,
        http_client: &reqwest::Client,
    ) -> Result<OAuth2RefreshToken, human_errors::Error> {
        let client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(self.client_id.clone()))
            .set_client_secret(oauth2::ClientSecret::new(self.client_secret.clone()))
            .set_auth_uri(oauth2::AuthUrl::new(self.auth_url.clone()).or_system_err(&[])?)
            .set_token_uri(oauth2::TokenUrl::new(self.token_url.clone()).or_system_err(&[])?);

        if !token_entry.needs_refresh() {
            return Ok(token_entry.clone());
        }

        let token_result = client
            .exchange_refresh_token(&oauth2::RefreshToken::new(
                token_entry.refresh_token.clone(),
            ))
            .request_async(http_client)
            .await
            .wrap_user_err(
                format!("Failed to refresh OAuth access token for {}.", &self.name),
                &[
                    "Ensure that your OAuth credentials are correct.",
                    "Check your network connection.",
                    "Try authenticating again by visiting /oauth/{provider}/setup.",
                ],
            )?;

        Ok(OAuth2RefreshToken {
            access_token: token_result.access_token().secret().to_string(),
            refresh_token: token_result
                .refresh_token()
                .map(|t| t.secret().to_string())
                .unwrap_or(token_entry.refresh_token.clone()),
            expires_at: chrono::Utc::now()
                + chrono::Duration::seconds(
                    token_result
                        .expires_in()
                        .unwrap_or(std::time::Duration::from_secs(3600))
                        .as_secs() as i64,
                ),
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct OAuth2RefreshToken {
    access_token: String,
    refresh_token: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

impl OAuth2RefreshToken {
    pub fn needs_refresh(&self) -> bool {
        chrono::Utc::now() + chrono::Duration::minutes(5) >= self.expires_at
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }
}

#[cfg(test)]
mod tests {
    // Import the actix test utilities under an alias: a bare `test` import would
    // shadow the built-in `#[test]` attribute with actix's async test macro.
    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::test as actix_test;
    use actix_web::{App, web};

    use crate::config::Config;
    use crate::db::SqliteDatabase;
    use crate::filter::Filter;
    use crate::services::ServicesContainer;

    #[test]
    fn state_matches_requires_both_present_nonempty_and_equal() {
        assert!(state_matches(Some("abc"), Some("abc")));
        assert!(!state_matches(Some("abc"), Some("def")));
        assert!(!state_matches(None, Some("abc")));
        assert!(!state_matches(Some("abc"), None));
        assert!(!state_matches(Some(""), Some("")));
    }

    #[test]
    fn oauth2_config_parses_optional_acl() {
        let with_acl: OAuth2Config = toml::from_str(
            r#"
                name = "Spotify"
                client_id = "x"
                client_secret = "y"
                auth_url = "https://accounts.spotify.com/authorize"
                token_url = "https://accounts.spotify.com/api/token"
                acl = 'client_ip in ["127.0.0.1"]'
            "#,
        )
        .unwrap();
        assert!(with_acl.acl.is_some());

        let without_acl: OAuth2Config = toml::from_str(
            r#"
                name = "Spotify"
                client_id = "x"
                client_secret = "y"
                auth_url = "https://accounts.spotify.com/authorize"
                token_url = "https://accounts.spotify.com/api/token"
            "#,
        )
        .unwrap();
        assert!(without_acl.acl.is_none());
    }

    /// Services with a single OAuth provider configured, the given admin ACL, and
    /// an optional provider-specific ACL.
    async fn service_with_provider(
        admin_acl: &str,
        provider_acl: Option<&str>,
    ) -> ServicesContainer<SqliteDatabase> {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let mut config = Config::default();
        config.web.admin.acl = Filter::new(admin_acl).unwrap();
        // A fixed base URL so the callback doesn't depend on a Host header.
        config.web.base_url = Some("http://localhost:8080".to_string());
        config.oauth2.insert(
            "spotify".to_string(),
            OAuth2Config {
                name: "Spotify".to_string(),
                jobs: vec![],
                acl: provider_acl.map(|a| Filter::new(a).unwrap()),
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                auth_url: "https://accounts.spotify.com/authorize".to_string(),
                token_url: "https://accounts.spotify.com/api/token".to_string(),
                scopes: vec![],
            },
        );
        ServicesContainer::new(config, db)
    }

    async fn wizard_home_status(services: ServicesContainer<SqliteDatabase>) -> StatusCode {
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(services))
                .service(configure::<ServicesContainer<SqliteDatabase>>()),
        )
        .await;
        let req = actix_test::TestRequest::get()
            .uri("/oauth/spotify/")
            .to_request();
        actix_test::call_service(&app, req).await.status()
    }

    #[actix_web::test]
    async fn setup_wizard_requires_admin_auth_by_default() {
        // No provider ACL, deny-all admin ACL, OIDC off ⇒ access denied (403).
        assert_eq!(
            wizard_home_status(service_with_provider("false", None).await).await,
            StatusCode::FORBIDDEN
        );
    }

    #[actix_web::test]
    async fn setup_wizard_provider_acl_allows_anyone() {
        // A provider ACL of `true` lets anyone connect even though the admin area
        // is locked down (admin ACL `false`, no sign-in).
        assert_eq!(
            wizard_home_status(service_with_provider("false", Some("true")).await).await,
            StatusCode::OK
        );
    }

    #[actix_web::test]
    async fn setup_wizard_provider_acl_can_deny_despite_open_admin() {
        // A provider ACL overrides the admin ACL in both directions: here it denies
        // even though the admin ACL would allow.
        assert_eq!(
            wizard_home_status(service_with_provider("true", Some("false")).await).await,
            StatusCode::FORBIDDEN
        );
    }

    #[actix_web::test]
    async fn setup_wizard_callback_rejects_missing_state() {
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(service_with_provider("true", None).await))
                .service(configure::<ServicesContainer<SqliteDatabase>>()),
        )
        .await;

        // Auth passes, but with no state cookie the CSRF check must reject the
        // callback before any authorization-code exchange is attempted.
        let req = actix_test::TestRequest::get()
            .uri("/oauth/spotify/callback?code=abc&state=xyz")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
