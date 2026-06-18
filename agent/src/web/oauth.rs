use crate::prelude::*;
use actix_web::{HttpResponse, dev::HttpServiceFactory, web};
use oauth2::{CsrfToken, Scope, TokenResponse};
use reqwest::Url;
use serde::Deserialize;

use crate::prelude::Services;

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

pub fn configure<S: Services + Send + Sync + 'static>() -> impl HttpServiceFactory {
    web::scope("/oauth/{provider}")
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
                    Ok(url) => actix_web::HttpResponse::Found()
                        .append_header((actix_web::http::header::LOCATION, url.to_string()))
                        .finish(),
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
    if let Some(base_url) =
        super::helpers::request::base_url(services.as_ref(), req.headers(), req.uri().scheme_str())
    {
        if let Some(config) = services.config().oauth2.get(&*provider).cloned() {
            if let Some(code) = query.get("code") {
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
                                return error_page(
                                    500,
                                    "Internal Server Error",
                                    "Failed to store OAuth token, please try again later.",
                                );
                            }
                        }

                        html_page(
                            200,
                            &format!("{} | Automate", config.name),
                            "Login Complete",
                            &format!(
                                "You have successfully completed setting up {}, you can close this window.",
                                config.name
                            ),
                        )
                    }
                    Err(e) => {
                        error!("OAuth callback handling failed: {}", e);
                        sentry::capture_error(&e);
                        return error_page(
                            500,
                            "Internal Server Error",
                            "Failed to complete OAuth token exchange.",
                        );
                    }
                }
            } else {
                return error_page(
                    400,
                    "Bad Request",
                    "Missing 'code' parameter in OAuth callback.",
                );
            }
        } else {
            return error_page(400, "Bad Request", "Invalid OAuth provider specified.");
        }
    } else {
        error_page(
            400,
            "Bad Request",
            "Your request did not include the required Host header.",
        )
    }
}

#[derive(Clone, Deserialize)]
pub struct OAuth2Config {
    pub name: String,

    #[serde(default)]
    pub jobs: Vec<String>,

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
    pub fn get_login_url(&self, redirect_url: impl ToString) -> Result<Url, human_errors::Error> {
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

        let (url, _csrf) = client
            .authorize_url(CsrfToken::new_random)
            .add_scopes(self.scopes.iter().cloned().map(Scope::new))
            .url()
            .clone();
        Ok(url)
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
    use super::*;
    use oauth2::{RequestTokenError, StandardErrorResponse, basic::BasicErrorResponseType};

    type RefreshError = RequestTokenError<std::io::Error, StandardErrorResponse<BasicErrorResponseType>>;

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
}
