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
