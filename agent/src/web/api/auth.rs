//! Public OIDC endpoints that drive the browser's popup login.
//!
//! The SPA runs the Authorization Code request itself (in a popup) and then calls
//! these endpoints: [`metadata`] tells it where the provider's authorization
//! endpoint is (and the `client_id`/`scopes` needed to build the request),
//! [`auth_token`] swaps the returned `code` for tokens using the server-held
//! `client_secret`, and [`auth_refresh`] renews an expired session. The issued
//! ID token is returned to the SPA as a bearer (never set as a cookie); the SPA
//! presents it as `Authorization: Bearer` on subsequent API requests.

use actix_web::{HttpResponse, web};
use serde::Deserialize;

use super::json_error;
use crate::prelude::*;
use crate::web::helpers::oidc::{discovery, exchange_code, refresh_tokens};

/// Request body for the authorization-code exchange.
#[derive(Deserialize)]
pub struct TokenExchangeRequest {
    code: String,
    redirect_uri: String,
}

/// Request body for a session renewal.
#[derive(Deserialize)]
pub struct TokenRefreshRequest {
    refresh_token: String,
}

/// `GET /api/v1/auth/metadata` — the parameters the SPA needs to begin a login:
/// the provider's authorization endpoint (resolved from the cached discovery
/// document so the browser never calls the provider cross-origin), the
/// `client_id`, and the requested `scopes` (always including `openid`). Public.
pub async fn metadata<S: Services>(services: web::Data<S>) -> HttpResponse {
    let config = services.config();
    let Some(oidc) = config.web.admin.oidc.as_ref() else {
        return json_error(
            actix_web::http::StatusCode::NOT_FOUND,
            "Administrative sign-in is not configured on this server.",
        );
    };

    let discovery = match discovery(services.as_ref(), oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document for auth metadata: {e}");
            services.session().record_error(&e);
            return json_error(
                actix_web::http::StatusCode::BAD_GATEWAY,
                "We could not reach the configured identity provider.",
            );
        }
    };

    let mut scopes = vec!["openid".to_string()];
    for scope in &oidc.scopes {
        if scope != "openid" {
            scopes.push(scope.clone());
        }
    }

    HttpResponse::Ok().json(serde_json::json!({
        "authorization_endpoint": discovery.authorization_endpoint,
        "client_id": oidc.client_id,
        "scopes": scopes,
    }))
}

/// `POST /api/v1/auth/token` — exchanges an authorization code for tokens using
/// the server-held client secret, returning the `id_token` (and refresh token,
/// when issued) to the SPA. Public (it is the login step).
pub async fn auth_token<S: Services>(
    services: web::Data<S>,
    body: web::Json<TokenExchangeRequest>,
) -> HttpResponse {
    let config = services.config();
    let Some(oidc) = config.web.admin.oidc.as_ref() else {
        return json_error(
            actix_web::http::StatusCode::NOT_FOUND,
            "Administrative sign-in is not configured on this server.",
        );
    };

    let discovery = match discovery(services.as_ref(), oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document during token exchange: {e}");
            services.session().record_error(&e);
            return json_error(
                actix_web::http::StatusCode::BAD_GATEWAY,
                "We could not reach the configured identity provider.",
            );
        }
    };

    match exchange_code(
        oidc,
        &discovery,
        &body.code,
        &body.redirect_uri,
        &services.http_client(),
    )
    .await
    {
        Ok(tokens) => HttpResponse::Ok().json(token_response(&tokens)),
        Err(e) => {
            warn!("OIDC token exchange failed: {e}");
            json_error(
                actix_web::http::StatusCode::BAD_GATEWAY,
                "The sign-in could not be completed. Please try signing in again.",
            )
        }
    }
}

/// `POST /api/v1/auth/refresh` — renews a session from a refresh token, returning
/// a fresh `id_token` (and rotated refresh token, when the provider issues one).
/// Public: a refresh token is the only credential required, and the agent
/// re-validates the resulting ID token on subsequent requests.
pub async fn auth_refresh<S: Services>(
    services: web::Data<S>,
    body: web::Json<TokenRefreshRequest>,
) -> HttpResponse {
    let config = services.config();
    let Some(oidc) = config.web.admin.oidc.as_ref() else {
        return json_error(
            actix_web::http::StatusCode::NOT_FOUND,
            "Administrative sign-in is not configured on this server.",
        );
    };

    let discovery = match discovery(services.as_ref(), oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document during refresh: {e}");
            services.session().record_error(&e);
            return json_error(
                actix_web::http::StatusCode::BAD_GATEWAY,
                "We could not reach the configured identity provider.",
            );
        }
    };

    match refresh_tokens(
        oidc,
        &discovery,
        &body.refresh_token,
        &services.http_client(),
    )
    .await
    {
        Ok(tokens) => HttpResponse::Ok().json(token_response(&tokens)),
        Err(e) => {
            warn!("OIDC token refresh failed: {e}");
            json_error(
                actix_web::http::StatusCode::UNAUTHORIZED,
                "Your session could not be renewed. Please sign in again.",
            )
        }
    }
}

/// The JSON body returned for a successful token exchange or refresh.
/// `refresh_token` is omitted when the provider did not issue one.
fn token_response(tokens: &crate::web::helpers::oidc::TokenSet) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "token".into(),
        serde_json::Value::String(tokens.id_token.clone()),
    );
    if let Some(refresh) = &tokens.refresh_token {
        body.insert(
            "refresh_token".into(),
            serde_json::Value::String(refresh.clone()),
        );
    }
    serde_json::Value::Object(body)
}
