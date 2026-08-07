//! The YNAB OAuth application people connect their own account through.
//!
//! YNAB used to be reached with one Personal Access Token pasted into the
//! configuration file. That works while an installation has a single owner and
//! stops working the moment it does not: the token belongs to whoever generated
//! it, every budget it can reach is theirs, and there is nothing to revoke per
//! person.
//!
//! This integration is the same shape as [`super::todoist`]: the operator
//! registers one application, and each person connects their own account
//! through it. Unlike Todoist there is no webhook half — YNAB does not deliver
//! events — so a grant is only ever used by a scheduled workflow.
//!
//! # Why the OAuth flow is written out rather than driven by a client library
//!
//! `rust_ynab` carries an optional OAuth client, but it mints its own CSRF state
//! and PKCE verifier and builds its own HTTP client, none of which fits the
//! agent: the state has to be the one [`super::PendingAuthorizations`] recorded,
//! or the callback cannot say whose authorisation it completes. The two
//! requests are small, so they are made directly.

use std::collections::HashMap;

use chrono::{Duration, Utc};
use oauth2::CsrfToken;
use serde_json::{Map, Value};

use automate_api::{ConnectionId, ConnectionStatus};

use super::{
    Connection, Integration, IntegrationContext, IntegrationInfo, SetupComplete, SetupRedirect,
};
use crate::config::{Config, YnabAppConfig};
use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::prelude::*;

/// The provider name under which YNAB accounts are linked.
pub const YNAB_PROVIDER: &str = "ynab";

/// The YNAB web app, which serves both the authorisation and token endpoints.
const APP_URL: &str = "https://app.ynab.com";

/// The API, which is deliberately on a different host from the two above.
const API_URL: &str = "https://api.ynab.com/v1";

/// The metadata key holding the YNAB user id a grant belongs to.
///
/// Not a credential, and — since YNAB tells us nothing else about the account,
/// not even an email address — the only thing distinguishing one linked account
/// from another.
pub const ACCOUNT_USER: &str = "user";

/// How long before expiry a token is renewed.
///
/// YNAB's access tokens last two hours, and a run that starts just inside the
/// window would otherwise make its first call with a token that expires
/// mid-run.
const RENEW_BEFORE: Duration = Duration::minutes(5);

/// How long an access token lasts when YNAB does not say.
const DEFAULT_LIFETIME: i64 = 7200;

pub struct YnabAppIntegration;

crate::register_integration!(YnabAppIntegration);

/// The configured application, if this instance has one.
pub fn app(config: &Config) -> Option<&YnabAppConfig> {
    config.connections.ynab.app.as_ref()
}

fn require_app(config: &Config) -> Result<&YnabAppConfig, human_errors::Error> {
    app(config).ok_or_else(|| {
        human_errors::user(
            "No YNAB application is configured on this Automate instance.",
            &["Add a [connections.ynab.app] section to your configuration."],
        )
    })
}

fn authorize_url(app: &YnabAppConfig) -> String {
    format!(
        "{}/oauth/authorize",
        app.app_url.as_deref().unwrap_or(APP_URL)
    )
}

fn token_url(app: &YnabAppConfig) -> String {
    format!("{}/oauth/token", app.app_url.as_deref().unwrap_or(APP_URL))
}

/// The API base a client should address, which is only ever overridden to point
/// the agent at a stand-in for YNAB.
pub fn api_url(config: &Config) -> Option<String> {
    app(config).and_then(|app| app.api_url.clone())
}

/// What YNAB returns from the token endpoint.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

impl TokenResponse {
    /// The credential to store, keeping `previous_refresh_token` when the
    /// response omits one — YNAB rotates the refresh token on every use, so a
    /// response without one leaves the one we already hold as the only way back.
    fn into_secret(self, previous_refresh_token: Option<String>) -> ConnectionSecret {
        ConnectionSecret::OAuth2 {
            access_token: self.access_token,
            refresh_token: self
                .refresh_token
                .or(previous_refresh_token)
                .unwrap_or_default(),
            expires_at: Utc::now() + Duration::seconds(self.expires_in.unwrap_or(DEFAULT_LIFETIME)),
        }
    }
}

/// The token endpoint's answer.
enum Grant {
    Issued(TokenResponse),

    /// YNAB will not honour this authorisation again — the refresh token has
    /// been used, revoked, or the app's credentials no longer match. Only the
    /// person who granted it can fix that, which is why it is a distinct answer
    /// rather than an error: a run that retries it on every schedule would fail
    /// forever with nothing to say why.
    Rejected(String),
}

/// Posts a form to YNAB's token endpoint and reads the grant back.
async fn exchange(
    http: &reqwest::Client,
    app: &YnabAppConfig,
    form: &[(&str, &str)],
) -> Result<Grant, human_errors::Error> {
    let response = http
        .post(token_url(app))
        .form(form)
        .send()
        .await
        .wrap_user_err(
            "Failed to exchange the YNAB authorization for an access token.",
            &[
                "Check that your YNAB client ID and secret are correct.",
                "Check your network connection.",
            ],
        )?;

    if response.status().is_client_error() {
        // YNAB names the reason in an `error` field; a body that does not is
        // still worth passing on, being the only account of what happened.
        let body = response.text().await.unwrap_or_default();
        let reason = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|body| {
                body.get("error")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| body.chars().take(200).collect());

        return Ok(Grant::Rejected(reason));
    }

    response
        .error_for_status()
        .wrap_system_err(
            "YNAB could not issue an access token.",
            &["This is usually temporary, and the operation will be retried."],
        )?
        .json()
        .await
        .wrap_system_err(
            "YNAB's token response was not in the format we expected.",
            &["This is likely a change on YNAB's side; please report it."],
        )
        .map(Grant::Issued)
}

/// Renews a grant that is about to expire, storing the rotated credential.
///
/// Returns the access token to use. A grant with no refresh token cannot be
/// renewed and is returned as-is; the stored token may still have life in it,
/// and refusing to use it would turn a missing configuration into a run that
/// cannot happen.
pub async fn access_token(
    services: &impl Services,
    connection: &crate::connections::Connection,
    secret: ConnectionSecret,
) -> Result<String, human_errors::Error> {
    let ConnectionSecret::OAuth2 {
        access_token,
        refresh_token,
        expires_at,
    } = secret
    else {
        return Err(human_errors::system(
            format!(
                "The connection '{}' does not hold a YNAB OAuth grant.",
                connection.id
            ),
            &["Reconnect the account."],
        ));
    };

    if refresh_token.is_empty() || expires_at > Utc::now() + RENEW_BEFORE {
        return Ok(access_token);
    }

    let config = services.config();
    let Some(app) = app(&config) else {
        return Ok(access_token);
    };

    let store = ConnectionStore::for_services(services);

    let refreshed = match exchange(
        &services.http_client(),
        app,
        &[
            ("client_id", app.client_id.as_str()),
            ("client_secret", app.client_secret.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
        ],
    )
    .await?
    {
        Grant::Issued(refreshed) => refreshed,
        Grant::Rejected(reason) => {
            // Marked rather than only reported, so the connections page offers
            // Reconnect instead of leaving somebody to work out from a failing
            // workflow that their authorisation is the thing that is wrong.
            store
                .set_status(connection.id, ConnectionStatus::NeedsReauthorization)
                .await?;

            return Err(human_errors::user(
                format!("YNAB will no longer accept this account's authorization ({reason})."),
                &["Reconnect your YNAB account from the connections page."],
            ));
        }
    };

    let renewed = refreshed.access_token.clone();

    store
        .update_secret(connection.id, refreshed.into_secret(Some(refresh_token)))
        .await?;

    debug!(connection.id = %connection.id, "Renewed the YNAB access token.");

    Ok(renewed)
}

impl YnabAppIntegration {
    /// Who the token belongs to, according to YNAB rather than to the request
    /// that carried the code.
    async fn identify(
        config: &Config,
        access_token: &str,
    ) -> Result<uuid::Uuid, human_errors::Error> {
        let client = rust_ynab::Client::new(access_token).wrap_system_err(
            "We could not initialise the YNAB API client.",
            &["This is unexpected; please report it."],
        )?;

        let client = match api_url(config) {
            Some(url) => client.with_base_url(url).wrap_user_err(
                "The configured YNAB API URL is not a valid URL.",
                &["Check `api_url` in your [connections.ynab.app] section."],
            )?,
            None => client.with_base_url(API_URL).wrap_system_err(
                "We could not initialise the YNAB API client.",
                &["This is unexpected; please report it."],
            )?,
        };

        client
            .get_user()
            .await
            .wrap_user_err(
                "YNAB would not tell us which account this authorization belongs to.",
                &["Try connecting your YNAB account again."],
            )
            .map(|user| user.id)
    }
}

#[async_trait::async_trait]
impl Integration for YnabAppIntegration {
    fn instances(&self, config: &Config) -> Vec<IntegrationInfo> {
        app(config)
            .map(|_| IntegrationInfo {
                id: YNAB_PROVIDER.to_string(),
                name: "YNAB".to_string(),
            })
            .into_iter()
            .collect()
    }

    fn acl(&self, config: &Config, _id: &str) -> Option<Filter> {
        app(config).and_then(|app| app.acl.clone())
    }

    async fn begin_setup(
        &self,
        id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<SetupRedirect, human_errors::Error> {
        let config = ctx.context.config();
        let app = require_app(&config)?;

        let state = CsrfToken::new_random().secret().clone();

        let url = format!(
            "{}?client_id={}&redirect_uri={}&response_type=code&state={}",
            authorize_url(app),
            urlencoding::encode(&app.client_id),
            urlencoding::encode(&ctx.callback_url(self, id)),
            urlencoding::encode(&state),
        );

        // Recorded before the visitor leaves, so the callback can establish
        // whose authorisation it completes without trusting the request.
        ctx.pending()
            .begin(&state, ctx.initiator.clone(), id)
            .await?;

        Ok(SetupRedirect { url, state })
    }

    async fn complete_setup(
        &self,
        id: &str,
        ctx: IntegrationContext<'_>,
        query: &HashMap<String, String>,
    ) -> Result<SetupComplete, human_errors::Error> {
        let config = ctx.context.config();
        let app = require_app(&config)?.clone();

        if let Some(error) = query.get("error") {
            return Err(human_errors::user(
                format!("YNAB did not complete the authorization: {error}."),
                &["Start connecting your YNAB account again."],
            ));
        }

        let code = query.get("code").ok_or_else(|| {
            human_errors::user(
                "YNAB did not return an authorization code.",
                &["Start connecting your YNAB account again."],
            )
        })?;

        let state = query.get("state").ok_or_else(|| {
            human_errors::user(
                "YNAB did not return the state we sent it.",
                &["Start connecting your YNAB account again from this application."],
            )
        })?;

        // Whose authorisation this is comes from what we recorded when the flow
        // began, never from the request: a callback arrives from YNAB, so
        // nothing in it can be trusted to name an account here.
        let owner = ctx.pending().claim(state, id).await?.tenant;

        let http = ctx.services().http_client();
        let token = match exchange(
            &http,
            &app,
            &[
                ("client_id", app.client_id.as_str()),
                ("client_secret", app.client_secret.as_str()),
                ("redirect_uri", ctx.callback_url(self, id).as_str()),
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
            ],
        )
        .await?
        {
            Grant::Issued(token) => token,
            Grant::Rejected(reason) => {
                return Err(human_errors::user(
                    format!("YNAB rejected the authorization ({reason})."),
                    &[
                        "Check that [connections.ynab.app] client_id and client_secret match the application in YNAB's Developer Settings.",
                        "Check that this instance's callback URL is registered as a redirect URI on that application.",
                    ],
                ));
            }
        };

        let user = Self::identify(&config, &token.access_token).await?;

        let mut metadata = Map::new();
        metadata.insert(ACCOUNT_USER.to_string(), Value::String(user.to_string()));

        let connections = ConnectionStore::new(ctx.for_tenant(owner.clone()), owner);

        // Keyed on the YNAB user id, which is the only thing YNAB tells us about
        // the account and the only thing that stays put.
        match connections
            .find_by_account(YNAB_PROVIDER, &user.to_string())
            .await?
        {
            Some(existing) => {
                connections
                    .update_secret(existing.id, token.into_secret(None))
                    .await?;
                connections.update_metadata(existing.id, metadata).await?;
                info!(connection.id = %existing.id, "Refreshed an existing YNAB connection.");
            }
            None => {
                let created = connections
                    .create_with_metadata(
                        YNAB_PROVIDER,
                        "YNAB",
                        Some(user.to_string()),
                        token.into_secret(None),
                        metadata,
                    )
                    .await?;
                info!(connection.id = %created.id, "Linked a YNAB account.");
            }
        }

        Ok(SetupComplete {
            heading: "YNAB connected".to_string(),
            message: "Your YNAB account is connected, you can close this window.".to_string(),
        })
    }

    async fn connections(
        &self,
        _id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<Vec<Connection>, human_errors::Error> {
        let store = ConnectionStore::new(ctx.services(), ctx.initiator.clone());

        Ok(store
            .list_for_provider(YNAB_PROVIDER)
            .await?
            .into_iter()
            .map(|connection| {
                // The credential itself never leaves the agent. When it next
                // needs renewing is useful to show; the token is not.
                let mut summary = Connection::new(connection.id.to_string(), connection.name)
                    .with_kind(connection.kind.as_str().to_string());

                if let Some(expires_at) = connection.expires_at {
                    summary = summary.with_detail(format!(
                        "Renews {}",
                        expires_at.format("%Y-%m-%d %H:%M UTC")
                    ));
                }

                summary
            })
            .collect())
    }

    async fn disconnect(
        &self,
        _id: &str,
        connection: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<(), human_errors::Error> {
        let store = ConnectionStore::new(ctx.services(), ctx.initiator.clone());

        let connection_id: ConnectionId = connection.parse().map_err(|err| {
            human_errors::user(
                format!("'{connection}' is not a connection identifier. {err}"),
                &["Use the identifier shown against the connection you want to remove."],
            )
        })?;

        // Checking the provider stops a connection being removed through
        // another integration's endpoint.
        match store.get(connection_id).await? {
            Some(existing) if existing.provider == YNAB_PROVIDER => {
                store.delete(connection_id).await?;
                info!("Disconnected the YNAB account '{connection}'.");
                Ok(())
            }
            _ => Err(human_errors::user(
                format!("There is no YNAB connection named '{connection}'."),
                &["It may already have been removed."],
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::AppContext;

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    async fn context() -> AppContext {
        context_at(None, None).await
    }

    /// A context whose YNAB calls are addressed at the given stand-ins.
    async fn context_at(app_url: Option<String>, api_url: Option<String>) -> AppContext {
        AppContext::new_mock(move |config| {
            config.connections.ynab.app = Some(YnabAppConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                app_url: app_url.clone(),
                api_url: api_url.clone(),
                acl: None,
            });
        })
        .await
        .unwrap()
    }

    fn ctx(context: &AppContext, initiator: TenantId) -> IntegrationContext<'_> {
        IntegrationContext {
            context,
            initiator,
            base_url: "https://automate.example.com",
        }
    }

    async fn link(context: &AppContext, user: &str) -> ConnectionId {
        let store = ConnectionStore::new(context.tenant(alice()), alice());
        let mut metadata = Map::new();
        metadata.insert(ACCOUNT_USER.to_string(), Value::String(user.to_string()));

        store
            .create_with_metadata(
                YNAB_PROVIDER,
                "YNAB",
                Some(user.to_string()),
                ConnectionSecret::OAuth2 {
                    access_token: "access-tYqR9".into(),
                    refresh_token: "refresh-Kx3Lm".into(),
                    expires_at: "2030-01-01T00:00:00Z".parse().unwrap(),
                },
                metadata,
            )
            .await
            .unwrap()
            .id
    }

    /// The integration is only offered when an application has been registered;
    /// otherwise the wizard would send people to a YNAB page that refuses them.
    #[tokio::test]
    async fn the_integration_is_only_listed_when_an_app_is_configured() {
        let configured = context().await;
        assert_eq!(YnabAppIntegration.instances(&configured.config()).len(), 1);

        let bare = AppContext::new_mock(|_| {}).await.unwrap();
        assert!(YnabAppIntegration.instances(&bare.config()).is_empty());
    }

    /// The credential must never leave the agent.
    #[tokio::test]
    async fn a_connection_never_carries_the_credential() {
        let context = context().await;
        link(&context, "1c3b8a5e-3b1e-4e4f-9a1b-2f6c4d5e6f70").await;

        let connections = YnabAppIntegration
            .connections(YNAB_PROVIDER, ctx(&context, alice()))
            .await
            .unwrap();

        let serialized = serde_json::to_string(&connections).unwrap();
        assert!(
            !serialized.contains("access-tYqR9"),
            "leaked an access token"
        );
        assert!(
            !serialized.contains("refresh-Kx3Lm"),
            "leaked a refresh token"
        );
    }

    /// A grant that is still good is used as it stands, so an ordinary run does
    /// not spend a token exchange it does not need — and, more importantly, does
    /// not rotate a refresh token for no reason.
    #[tokio::test]
    async fn a_live_token_is_used_without_being_renewed() {
        let context = context().await;
        let id = link(&context, "1c3b8a5e-3b1e-4e4f-9a1b-2f6c4d5e6f70").await;

        let services = context.tenant(alice());
        let store = ConnectionStore::new(services.clone(), alice());
        let connection = store.get(id).await.unwrap().unwrap();
        let secret = store.open(&connection).unwrap();

        assert_eq!(
            access_token(&services, &connection, secret).await.unwrap(),
            "access-tYqR9"
        );
    }

    /// A grant is renewed before it lapses, and the rotated pair is stored so
    /// the next run starts from it rather than repeating the exchange.
    #[tokio::test]
    async fn an_expiring_grant_is_renewed_and_the_rotated_pair_is_stored() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let ynab = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-2",
                "refresh_token": "refresh-2",
                "expires_in": 7200,
                "token_type": "bearer",
            })))
            .mount(&ynab)
            .await;

        let context = context_at(Some(ynab.uri()), None).await;
        let services = context.tenant(alice());
        let store = ConnectionStore::new(services.clone(), alice());

        let connection = store
            .create(
                YNAB_PROVIDER,
                "YNAB",
                Some("1c3b8a5e-3b1e-4e4f-9a1b-2f6c4d5e6f70".into()),
                ConnectionSecret::OAuth2 {
                    access_token: "access-1".into(),
                    refresh_token: "refresh-1".into(),
                    expires_at: Utc::now() + Duration::minutes(1),
                },
            )
            .await
            .unwrap();

        let secret = store.open(&connection).unwrap();
        assert_eq!(
            access_token(&services, &connection, secret).await.unwrap(),
            "access-2"
        );

        let stored = store.get(connection.id).await.unwrap().unwrap();
        let ConnectionSecret::OAuth2 {
            access_token,
            refresh_token,
            ..
        } = store.open(&stored).unwrap()
        else {
            panic!("the renewed credential should still be an OAuth grant");
        };

        assert_eq!(access_token, "access-2");
        assert_eq!(refresh_token, "refresh-2");
    }

    /// A refresh YNAB refuses is not something a retry can fix, so the
    /// connection is marked rather than left to fail on every schedule.
    #[tokio::test]
    async fn a_refused_refresh_marks_the_connection_for_reauthorization() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let ynab = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "invalid_grant",
            })))
            .mount(&ynab)
            .await;

        let context = context_at(Some(ynab.uri()), None).await;
        let services = context.tenant(alice());
        let store = ConnectionStore::new(services.clone(), alice());

        let connection = store
            .create(
                YNAB_PROVIDER,
                "YNAB",
                Some("1c3b8a5e-3b1e-4e4f-9a1b-2f6c4d5e6f70".into()),
                ConnectionSecret::OAuth2 {
                    access_token: "access-1".into(),
                    refresh_token: "refresh-1".into(),
                    expires_at: Utc::now() + Duration::minutes(1),
                },
            )
            .await
            .unwrap();

        let secret = store.open(&connection).unwrap();
        access_token(&services, &connection, secret)
            .await
            .expect_err("a refused refresh should be reported");

        let stored = store.get(connection.id).await.unwrap().unwrap();
        assert_eq!(stored.status, ConnectionStatus::NeedsReauthorization);
    }

    /// The whole flow: the state we recorded decides whose account this is, the
    /// code is exchanged, and YNAB is asked who it belongs to.
    #[tokio::test]
    async fn completing_an_authorization_links_the_account_that_started_it() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let ynab = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-1",
                "refresh_token": "refresh-1",
                "expires_in": 7200,
                "token_type": "bearer",
            })))
            .mount(&ynab)
            .await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "user": { "id": "1c3b8a5e-3b1e-4e4f-9a1b-2f6c4d5e6f70" } }
            })))
            .mount(&ynab)
            .await;

        let context = context_at(Some(ynab.uri()), Some(ynab.uri())).await;
        let redirect = YnabAppIntegration
            .begin_setup(YNAB_PROVIDER, ctx(&context, alice()))
            .await
            .unwrap();

        let query = HashMap::from([
            ("code".to_string(), "the-code".to_string()),
            ("state".to_string(), redirect.state.clone()),
        ]);

        YnabAppIntegration
            // A different initiator, to prove the account comes from what was
            // recorded rather than from the request the callback arrived on.
            .complete_setup(
                YNAB_PROVIDER,
                ctx(&context, TenantId::new("mallory").unwrap()),
                &query,
            )
            .await
            .unwrap();

        let store = ConnectionStore::new(context.tenant(alice()), alice());
        let linked = store.list_for_provider(YNAB_PROVIDER).await.unwrap();
        assert_eq!(linked.len(), 1);
        assert_eq!(
            linked[0].account.as_deref(),
            Some("1c3b8a5e-3b1e-4e4f-9a1b-2f6c4d5e6f70")
        );

        let mallory = ConnectionStore::new(
            context.tenant(TenantId::new("mallory").unwrap()),
            TenantId::new("mallory").unwrap(),
        );
        assert!(
            mallory
                .list_for_provider(YNAB_PROVIDER)
                .await
                .unwrap()
                .is_empty(),
            "the credential was filed under the account that carried the callback"
        );
    }

    /// Re-authorising an account already linked refreshes it in place, so
    /// somebody reconnecting after an expiry is not left with two entries they
    /// cannot tell apart.
    #[tokio::test]
    async fn reauthorizing_the_same_account_does_not_create_a_second_connection() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let ynab = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-2",
                "refresh_token": "refresh-2",
                "expires_in": 7200,
                "token_type": "bearer",
            })))
            .mount(&ynab)
            .await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "user": { "id": "1c3b8a5e-3b1e-4e4f-9a1b-2f6c4d5e6f70" } }
            })))
            .mount(&ynab)
            .await;

        let context = context_at(Some(ynab.uri()), Some(ynab.uri())).await;
        let existing = link(&context, "1c3b8a5e-3b1e-4e4f-9a1b-2f6c4d5e6f70").await;

        let redirect = YnabAppIntegration
            .begin_setup(YNAB_PROVIDER, ctx(&context, alice()))
            .await
            .unwrap();

        YnabAppIntegration
            .complete_setup(
                YNAB_PROVIDER,
                ctx(&context, alice()),
                &HashMap::from([
                    ("code".to_string(), "the-code".to_string()),
                    ("state".to_string(), redirect.state.clone()),
                ]),
            )
            .await
            .unwrap();

        let store = ConnectionStore::new(context.tenant(alice()), alice());
        let linked = store.list_for_provider(YNAB_PROVIDER).await.unwrap();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].id, existing);
    }
}
