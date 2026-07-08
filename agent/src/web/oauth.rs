use crate::prelude::*;
use actix_web::cookie::time::Duration as CookieDuration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::{HttpRequest, HttpResponse, dev::HttpServiceFactory, web};
use oauth2::{CsrfToken, Scope, TokenResponse};
use reqwest::Url;
use serde::Deserialize;

use crate::prelude::Services;
use crate::web::helpers::oidc::AdminRequestFilter;
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

/// An HTML page shown when a visitor is not permitted to use a provider's
/// self-service wizard.
fn access_denied_page() -> HttpResponse {
    error_page(
        403,
        "Access denied",
        "You are not permitted to set up this integration.",
    )
}

/// An HTML page shown when an admin-gated wizard is opened directly (a top-level
/// navigation that cannot carry the admin bearer token). These wizards are
/// launched from the Automate admin area instead.
fn admin_only_page() -> HttpResponse {
    error_page(
        403,
        "Sign in required",
        "This integration is set up from the Automate admin area. Open the admin UI and start the connection from there.",
    )
}

/// The outcome of evaluating a request against a provider's *public* (top-level
/// navigation) wizard path.
enum PublicWizardOutcome {
    /// The visitor may proceed with the flow.
    Allowed,
    /// The visitor is not permitted by the applicable ACL.
    Denied,
    /// The provider is admin-gated and OIDC is configured, so it cannot be
    /// authorised on a top-level navigation (which carries no bearer token). It
    /// must be launched from the admin SPA via [`start`] instead.
    AdminOnly,
}

/// Decides whether a top-level navigation may use a provider's wizard.
///
/// A provider that defines its own `acl` is self-service: the ACL is evaluated
/// against request metadata (no `claims.*`, since a top-level navigation carries
/// no bearer). A provider without its own `acl` is admin-gated: when OIDC is
/// disabled the admin ACL is evaluated against request metadata exactly as before
/// (e.g. an IP allow-list still grants access); when OIDC is enabled the bearer
/// cannot ride a top-level navigation, so the flow must be started from the admin
/// SPA and this path reports [`PublicWizardOutcome::AdminOnly`].
fn public_wizard_outcome<S: Services>(
    services: &S,
    req: &HttpRequest,
    provider_config: &OAuth2Config,
) -> PublicWizardOutcome {
    let config = services.config();
    let admin = &config.web.admin;

    let acl = match provider_config.acl.as_ref() {
        Some(acl) => acl,
        None => {
            if admin.oidc.is_some() {
                return PublicWizardOutcome::AdminOnly;
            }
            &admin.acl
        }
    };

    let filter = AdminRequestFilter {
        method: req.method().as_str(),
        path: req.path(),
        client_ip: client_ip(config.web.trust_proxy, req.headers(), req.peer_addr()),
        headers: req.headers(),
        claims: None,
    };

    if acl.matches(&filter).unwrap_or(false) {
        PublicWizardOutcome::Allowed
    } else {
        PublicWizardOutcome::Denied
    }
}

/// `GET /api/v1/oauth` — lists the configured integration providers so the admin
/// SPA can offer a "connect" action for each. Admin-gated by `api_auth`.
pub async fn list_providers<S: Services>(services: web::Data<S>) -> HttpResponse {
    let providers: Vec<serde_json::Value> = services
        .config()
        .oauth2
        .iter()
        .map(|(key, cfg)| serde_json::json!({ "provider": key, "name": cfg.name }))
        .collect();

    HttpResponse::Ok().json(providers)
}

#[derive(Deserialize)]
pub struct StartRequest {
    /// Where the popup should return to once the connection completes. Optional;
    /// purely informational for the SPA.
    #[serde(default)]
    #[allow(dead_code)]
    return_to: Option<String>,
}

/// `POST /api/v1/oauth/{provider}/start` — mints a provider authorization URL for
/// the admin SPA to open in a popup, and sets the transient state cookie the
/// top-level `/oauth/{provider}/callback` later verifies. Admin-gated by
/// `api_auth`, so the caller is already an authorised administrator.
pub async fn start<S: Services + Send + Sync + 'static>(
    provider: web::Path<String>,
    services: web::Data<S>,
    req: HttpRequest,
    _body: Option<web::Json<StartRequest>>,
) -> HttpResponse {
    let Some(base_url) =
        super::helpers::request::base_url(services.as_ref(), req.headers(), req.uri().scheme_str())
    else {
        return super::api::json_error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "Could not determine the public base URL for the connection redirect.",
        );
    };

    let Some(cfg) = services.config().oauth2.get(&*provider).cloned() else {
        return super::api::json_error(
            actix_web::http::StatusCode::NOT_FOUND,
            "No such integration provider is configured.",
        );
    };

    match cfg.get_login_url(format!("{base_url}/oauth/{provider}/callback")) {
        Ok((url, state)) => {
            let secure = super::helpers::request::is_https(
                services.config().web.trust_proxy,
                req.headers(),
                req.uri().scheme_str(),
            );
            HttpResponse::Ok()
                .cookie(oauth_state_cookie(&provider, state, secure))
                .json(serde_json::json!({ "authorize_url": url.to_string() }))
        }
        Err(e) => {
            error!("Failed to build OAuth login URL: {}", e);
            sentry::capture_error(&e);
            super::api::json_error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to start the integration setup.",
            )
        }
    }
}

pub fn configure<S: Services + Send + Sync + 'static>() -> impl HttpServiceFactory {
    // The public wizard scope serves two things: self-service providers (which
    // opt in via their own `acl`) running the whole flow as top-level
    // navigations, and the OAuth `callback` that every flow — including the
    // admin SPA's popup, started via `/api/v1/oauth/{provider}/start` — is
    // redirected to by the provider. The callback is protected by the transient
    // state cookie rather than the admin gate, so it is reachable without a
    // bearer (which a cross-site top-level redirect could not carry anyway).
    web::scope("/oauth/{provider}")
        .route("/", web::get().to(oauth_home::<S>))
        .route("/authorize", web::get().to(oauth_authorize::<S>))
        .route("/callback", web::get().to(oauth_callback::<S>))
}

async fn oauth_home<S: Services + Send + Sync + 'static>(
    provider: web::Path<String>,
    services: web::Data<S>,
    req: HttpRequest,
) -> actix_web::HttpResponse {
    let Some(config) = services.config().oauth2.get(&*provider).cloned() else {
        return not_found();
    };

    match public_wizard_outcome(services.as_ref(), &req, &config) {
        PublicWizardOutcome::AdminOnly => admin_only_page(),
        PublicWizardOutcome::Denied => access_denied_page(),
        PublicWizardOutcome::Allowed => html_action_page(
            &format!("{} | Automate", config.name),
            &format!("Login with {}", config.name),
            &format!(
                "Click the button below to initiate the setup process for {}.",
                config.name
            ),
            &format!("/oauth/{}/authorize", &*provider),
            "Login",
        ),
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
                // This is a public, top-level navigation. Self-service providers
                // (with their own `acl`) are evaluated here; admin-gated providers
                // are reachable only when OIDC is off (via the admin ACL), and
                // otherwise must be launched from the admin SPA.
                match public_wizard_outcome(services.as_ref(), &req, &cfg) {
                    PublicWizardOutcome::AdminOnly => return admin_only_page(),
                    PublicWizardOutcome::Denied => return access_denied_page(),
                    PublicWizardOutcome::Allowed => {}
                }

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
                    sentry::capture_error(&e);
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

    /// The Todoist configuration used for the re-authorization reminder raised
    /// when this provider's refresh token expires. It is merged over the global
    /// `[connections.todoist]` configuration, so it only needs to specify the
    /// fields that should differ (for example a dedicated project or section).
    #[serde(default)]
    pub todoist: crate::config::TodoistConfig,
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

    /// Refreshes the stored access token, returning the refreshed token or
    /// [`TokenRefreshOutcome::Expired`] when the provider reports that the
    /// refresh token itself is no longer valid (`invalid_grant`) — meaning the
    /// user must re-authorize before the account can be used again.
    ///
    /// A token that does not yet need refreshing is returned unchanged. Any
    /// other failure (network error, 5xx, rate limiting) is surfaced as a
    /// transient system error so that the calling job retries.
    pub(crate) async fn refresh_access_token(
        &self,
        token_entry: &OAuth2RefreshToken,
        http_client: &reqwest::Client,
    ) -> Result<TokenRefreshOutcome, human_errors::Error> {
        if !token_entry.needs_refresh() {
            return Ok(TokenRefreshOutcome::Refreshed(token_entry.clone()));
        }

        let client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(self.client_id.clone()))
            .set_client_secret(oauth2::ClientSecret::new(self.client_secret.clone()))
            .set_auth_uri(oauth2::AuthUrl::new(self.auth_url.clone()).or_system_err(&[])?)
            .set_token_uri(oauth2::TokenUrl::new(self.token_url.clone()).or_system_err(&[])?);

        let token_result = match client
            .exchange_refresh_token(&oauth2::RefreshToken::new(
                token_entry.refresh_token.clone(),
            ))
            .request_async(http_client)
            .await
        {
            Ok(token_result) => token_result,
            // The provider rejected the refresh token itself: it has expired or
            // been revoked, so the account must be re-authorized.
            Err(err) if is_invalid_grant(&err) => return Ok(TokenRefreshOutcome::Expired),
            // Everything else (network failures, 5xx, rate limiting) is treated
            // as transient so the job retries and the failure reaches Sentry.
            Err(err) => {
                return Err(err).wrap_system_err(
                    format!("Failed to refresh OAuth access token for {}.", &self.name),
                    &[
                        "Ensure that your OAuth credentials are correct.",
                        "Check your network connection.",
                    ],
                );
            }
        };

        Ok(TokenRefreshOutcome::Refreshed(OAuth2RefreshToken {
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
        }))
    }
}

/// The result of attempting to refresh an OAuth2 access token.
pub(crate) enum TokenRefreshOutcome {
    /// The access token was refreshed (or did not yet need refreshing).
    Refreshed(OAuth2RefreshToken),
    /// The refresh token has expired or been revoked by the provider. The
    /// account must be re-authorized before it can be used again.
    Expired,
}

/// Returns `true` when a token-endpoint error indicates that the refresh token
/// is no longer valid (the OAuth2 `invalid_grant` error), as opposed to a
/// transient failure such as a network error or a 5xx response.
fn is_invalid_grant<RE>(
    err: &oauth2::RequestTokenError<
        RE,
        oauth2::StandardErrorResponse<oauth2::basic::BasicErrorResponseType>,
    >,
) -> bool
where
    RE: std::error::Error + 'static,
{
    matches!(
        err,
        oauth2::RequestTokenError::ServerResponse(resp)
            if matches!(resp.error(), oauth2::basic::BasicErrorResponseType::InvalidGrant)
    )
}

/// Refreshes the OAuth2 token for `provider`, transparently handling refresh
/// token expiry.
///
/// On success returns `Ok(Some(token))`. When the provider reports that the
/// refresh token has expired or been revoked this enqueues the re-authorization
/// workflow (which raises a Todoist reminder containing the re-authorization
/// link) and returns `Ok(None)` so the caller stops using the dead account.
/// Transient failures are returned as `Err` so the calling job retries.
pub async fn refresh_or_notify(
    provider: &str,
    token: &OAuth2RefreshToken,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<Option<OAuth2RefreshToken>, human_errors::Error> {
    let config = services.config().get_oauth2(provider)?;

    match config
        .refresh_access_token(token, &services.http_client())
        .await?
    {
        TokenRefreshOutcome::Refreshed(token) => Ok(Some(token)),
        TokenRefreshOutcome::Expired => {
            warn!(
                oauth.provider = provider,
                "The refresh token for '{provider}' has expired or been revoked; raising a re-authorization reminder and removing the account."
            );

            crate::jobs::OAuth2ReauthorizationRequiredWorkflow::dispatch(
                crate::jobs::OAuth2ReauthorizationRequiredConfig {
                    provider: provider.to_string(),
                    todoist: config.todoist.clone(),
                },
                Some(format!("oauth-reauth/{provider}").into()),
                services,
            )
            .await?;

            Ok(None)
        }
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
    use oauth2::{RequestTokenError, StandardErrorResponse, basic::BasicErrorResponseType};

    use crate::config::Config;
    use crate::db::SqliteDatabase;
    use crate::filter::Filter;
    use crate::services::ServicesContainer;

    type RefreshError =
        RequestTokenError<std::io::Error, StandardErrorResponse<BasicErrorResponseType>>;

    #[test]
    fn invalid_grant_is_treated_as_expiry() {
        let err: RefreshError = RequestTokenError::ServerResponse(StandardErrorResponse::new(
            BasicErrorResponseType::InvalidGrant,
            None,
            None,
        ));
        assert!(is_invalid_grant(&err));
    }

    #[test]
    fn other_server_errors_are_not_expiry() {
        // A different OAuth error code (e.g. a transient/configuration issue)
        // must not be misclassified as a permanently expired refresh token.
        let err: RefreshError = RequestTokenError::ServerResponse(StandardErrorResponse::new(
            BasicErrorResponseType::InvalidRequest,
            None,
            None,
        ));
        assert!(!is_invalid_grant(&err));
    }

    #[test]
    fn transport_errors_are_not_expiry() {
        // A network/transport failure is transient and must keep retrying rather
        // than tearing down the account.
        let err: RefreshError = RequestTokenError::Request(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset",
        ));
        assert!(!is_invalid_grant(&err));
    }

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
        ServicesContainer::new_custom_mock(|config, _| {
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
                    todoist: Default::default(),
                },
            );
        }).await.unwrap()
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

    /// Like [`service_with_provider`] but with admin OIDC configured (a
    /// syntactically valid endpoint that the public wizard gate never contacts —
    /// it only checks whether OIDC is present).
    async fn service_with_provider_and_oidc(
        admin_acl: &str,
        provider_acl: Option<&str>,
    ) -> ServicesContainer<SqliteDatabase> {
        ServicesContainer::new_custom_mock(|config, _| {
            config.web.admin.acl = Filter::new(admin_acl).unwrap();
            config.web.base_url = Some("http://localhost:8080".to_string());
            config.web.admin.oidc = Some(crate::config::OidcConfig {
                endpoint: "https://auth.example.com".to_string(),
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                scopes: vec![],
            });
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
                    todoist: Default::default(),
                },
            );
        })
        .await
        .unwrap()
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
    async fn setup_wizard_admin_gated_without_oidc_uses_admin_acl() {
        // No provider ACL and OIDC off ⇒ the admin ACL is evaluated on request
        // metadata exactly as before, so a permissive admin ACL grants access via
        // the public path.
        assert_eq!(
            wizard_home_status(service_with_provider("true", None).await).await,
            StatusCode::OK
        );
    }

    #[actix_web::test]
    async fn setup_wizard_admin_gated_with_oidc_is_admin_only() {
        // No provider ACL and OIDC on ⇒ a top-level navigation can't carry the
        // bearer, so the public path reports it must be launched from the admin
        // SPA (403) regardless of how permissive the admin ACL is.
        let services = service_with_provider_and_oidc("true", None).await;
        assert_eq!(wizard_home_status(services).await, StatusCode::FORBIDDEN);
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

        // The callback is public (the provider redirects to it on a top-level
        // navigation), protected solely by the transient state cookie: with no
        // cookie the CSRF check must reject it before any code exchange.
        let req = actix_test::TestRequest::get()
            .uri("/oauth/spotify/callback?code=abc&state=xyz")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
