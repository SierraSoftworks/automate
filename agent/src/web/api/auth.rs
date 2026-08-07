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
    let Some(oidc) = config.web.oidc() else {
        return json_error(
            actix_web::http::StatusCode::NOT_FOUND,
            "Administrative sign-in is not configured on this server.",
        );
    };

    let discovery = match discovery(services.as_ref(), oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document for auth metadata: {e}");
            services.session().record_human_error(&e);
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
    let Some(oidc) = config.web.oidc() else {
        return json_error(
            actix_web::http::StatusCode::NOT_FOUND,
            "Administrative sign-in is not configured on this server.",
        );
    };

    let discovery = match discovery(services.as_ref(), oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document during token exchange: {e}");
            services.session().record_human_error(&e);
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
    let Some(oidc) = config.web.oidc() else {
        return json_error(
            actix_web::http::StatusCode::NOT_FOUND,
            "Administrative sign-in is not configured on this server.",
        );
    };

    let discovery = match discovery(services.as_ref(), oidc).await {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to load OIDC discovery document during refresh: {e}");
            services.session().record_human_error(&e);
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

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test, web};
    use automate_api::TenantId;

    use crate::config::OidcConfig;
    use crate::services::AppContext;
    use crate::web::api::configure;

    fn provider() -> OidcConfig {
        OidcConfig {
            endpoint: "https://idp.example.com".into(),
            client_id: "a-client".into(),
            client_secret: "a-secret".into(),
            scopes: vec!["openid".into(), "email".into()],
            username_claim: None,
        }
    }

    macro_rules! app {
        ($context:expr) => {
            test::init_service(
                App::new()
                    .app_data(web::Data::new($context.tenant(TenantId::local())))
                    .app_data(web::Data::new($context.clone()))
                    .service(configure()),
            )
            .await
        };
    }

    /// Every endpoint in the login flow reads the provider from `[web.auth]`.
    #[actix_web::test]
    async fn signing_in_is_offered_when_the_provider_is_configured() {
        let context = AppContext::new_mock(|config| {
            config.web.auth.oidc = Some(provider());
        })
        .await
        .unwrap();

        let app = app!(context);

        for (method, uri) in [
            ("GET", "/api/v1/auth/metadata"),
            ("POST", "/api/v1/auth/token"),
            ("POST", "/api/v1/auth/refresh"),
        ] {
            let request = if method == "GET" {
                test::TestRequest::get().uri(uri).to_request()
            } else {
                // A body the handler will get past, so a 404 can only mean it
                // decided there was no provider at all.
                test::TestRequest::post()
                    .uri(uri)
                    .set_json(serde_json::json!({
                        "code": "x",
                        "redirect_uri": "https://example.com/",
                        "refresh_token": "x",
                    }))
                    .to_request()
            };

            let status = test::call_service(&app, request).await.status();

            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "{method} {uri} reported that sign-in is not configured, but it is",
            );
        }
    }

    #[actix_web::test]
    async fn an_installation_with_no_provider_says_sign_in_is_not_configured() {
        // The other side of it: the 404 has to still mean what it says, or the
        // test above would pass against a handler that never returns one.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let app = app!(context);

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/metadata")
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
