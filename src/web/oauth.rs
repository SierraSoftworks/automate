use crate::{
    prelude::*,
    web::ui::{error_page, not_found},
};
use actix_web::{dev::HttpServiceFactory, web};
use oauth2::TokenResponse;
use reqwest::Url;
use serde::Deserialize;
use yew::{ServerRenderer, html};

use crate::{prelude::Services, ui};

pub fn configure<S: Services + Send + Sync + 'static>() -> impl HttpServiceFactory {
    web::scope("/oauth/{provider}")
        .route("/setup", web::get().to(oauth_setup::<S>))
        .route("/callback", web::get().to(oauth_callback::<S>))
}

async fn oauth_setup<S: Services + Send + Sync + 'static>(
    provider: web::Path<String>,
    services: web::Data<S>,
    host: web::Header<HostHeader>,
) -> impl actix_web::Responder {
    match services.config().oauth2.get(&*provider).cloned() {
        Some(cfg) => {
            match cfg.get_login_url(format!(
                "{}/oauth/{provider}/callback",
                services
                    .config()
                    .web
                    .base_url
                    .as_deref()
                    .unwrap_or(&format!("https://{}", host.0.0))
            )) {
                Ok(url) => actix_web::HttpResponse::Found()
                    .append_header((actix_web::http::header::LOCATION, url.to_string()))
                    .finish(),
                Err(e) => {
                    error!("Failed to get OAuth login URL: {}", e);
                    error_page(
                        500,
                        "Internal Server Error",
                        "Failed to initiate OAuth login process.",
                    )
                    .await
                }
            }
        }
        None => not_found().await,
    }
}

async fn oauth_callback<S: Services + Send + Sync + 'static>(
    services: web::Data<S>,
    provider: web::Path<String>,
    query: web::Query<std::collections::HashMap<String, String>>,
) -> actix_web::HttpResponse {
    if let Some(config) = services.config().oauth2.get(&*provider).cloned() {
        if let Some(code) = query.get("code") {
            match config.handle_callback(code.clone()).await {
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
                            )
                            .await;
                        }
                    }

                    let renderer = ServerRenderer::<crate::ui::Page>::with_props(|| {
                        ui::PageProps {
                            title: None,
                            children: html! {
                                <ui::Center>
                                    <h1>{ "Login Complete" }</h1>
                                    <p>{ "You have successfully completed signing into your account, you can close this window." }</p>
                                </ui::Center>
                            },
                        }
                    });

                    let rendered = renderer.render().await;

                    actix_web::HttpResponse::Ok()
                        .content_type("text/html; charset=utf-8")
                        .body(format!("<!DOCTYPE html>{}", rendered))
                }
                Err(e) => {
                    error!("OAuth callback handling failed: {}", e);
                    return error_page(
                        500,
                        "Internal Server Error",
                        "Failed to complete OAuth token exchange.",
                    )
                    .await;
                }
            }
        } else {
            return error_page(
                400,
                "Bad Request",
                "Missing 'code' parameter in OAuth callback.",
            )
            .await;
        }
    } else {
        return error_page(400, "Bad Request", "Invalid OAuth provider specified.").await;
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
}

impl OAuth2Config {
    pub fn get_login_url(&self, redirect_url: impl ToString) -> Result<Url, human_errors::Error> {
        let client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(self.client_id.clone()))
            .set_client_secret(oauth2::ClientSecret::new(self.client_secret.clone()))
            .set_auth_uri(oauth2::AuthUrl::new(self.auth_url.clone()).map_err_as_system(&[])?)
            .set_token_uri(oauth2::TokenUrl::new(self.token_url.clone()).map_err_as_system(&[])?)
            // Set the URL the user will be redirected to after the authorization process.
            .set_redirect_uri(
                oauth2::RedirectUrl::new(redirect_url.to_string()).map_err_as_system(&[])?,
            );

        Ok(client.auth_uri().url().clone())
    }

    pub async fn handle_callback(
        &self,
        code: String,
    ) -> Result<OAuth2RefreshToken, human_errors::Error> {
        let client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(self.client_id.clone()))
            .set_client_secret(oauth2::ClientSecret::new(self.client_secret.clone()))
            .set_auth_uri(oauth2::AuthUrl::new(self.auth_url.clone()).map_err_as_system(&[])?)
            .set_token_uri(oauth2::TokenUrl::new(self.token_url.clone()).map_err_as_system(&[])?);

        let token_result = client
            .exchange_code(oauth2::AuthorizationCode::new(code))
            .request_async(&reqwest::Client::new())
            .await
            .wrap_err_as_user(
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
    ) -> Result<OAuth2RefreshToken, human_errors::Error> {
        let client = oauth2::basic::BasicClient::new(oauth2::ClientId::new(self.client_id.clone()))
            .set_client_secret(oauth2::ClientSecret::new(self.client_secret.clone()))
            .set_auth_uri(oauth2::AuthUrl::new(self.auth_url.clone()).map_err_as_system(&[])?)
            .set_token_uri(oauth2::TokenUrl::new(self.token_url.clone()).map_err_as_system(&[])?);

        if !token_entry.needs_refresh() {
            return Ok(token_entry.clone());
        }

        let http_client = reqwest::Client::new();

        let token_result = client
            .exchange_refresh_token(&oauth2::RefreshToken::new(
                token_entry.refresh_token.clone(),
            ))
            .request_async(&http_client)
            .await
            .wrap_err_as_user(
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

struct HostHeader(String);

impl actix_web::http::header::Header for HostHeader {
    fn name() -> actix_web::http::header::HeaderName {
        actix_web::http::header::HOST
    }

    fn parse<M: actix_web::HttpMessage>(msg: &M) -> Result<Self, actix_web::error::ParseError> {
        let header_value = msg
            .headers()
            .get(actix_web::http::header::HOST)
            .ok_or(actix_web::error::ParseError::Header)?;
        let header_str = header_value
            .to_str()
            .map_err(|_| actix_web::error::ParseError::Header)?;
        Ok(HostHeader(header_str.to_string()))
    }
}

impl actix_web::http::header::TryIntoHeaderValue for HostHeader {
    type Error = actix_web::http::header::InvalidHeaderValue;

    fn try_into_value(self) -> Result<actix_web::http::header::HeaderValue, Self::Error> {
        actix_web::http::header::HeaderValue::from_str(&self.0)
    }
}
