//! The Todoist OAuth application people connect their own account through.
//!
//! Todoist used to be reached with one API token pasted into the configuration
//! file. That works while an installation has a single owner and stops working
//! the moment it does not: the token belongs to whoever generated it, every
//! task lands in their account, and there is nothing to revoke per person.
//!
//! This integration is the same shape as [`super::github_app`]: the operator
//! registers one application, and each person connects their own account
//! through it. What the agent holds afterwards is a per-account grant, which is
//! also what makes `/webhooks/todoist` routable — Todoist names the account an
//! event belongs to, and the connection says whose it is.
//!
//! # Why the OAuth flow is written out rather than driven by the `oauth2` crate
//!
//! Todoist's authorisation endpoint takes its scopes comma-separated, where
//! RFC 6749 (and therefore the crate) uses spaces, and its token endpoint is on
//! a different host from the authorisation one. Both are small deviations and
//! both are load-bearing, so the two requests are made directly rather than
//! bent into a client that assumes the standard spelling.

use std::collections::HashMap;

use chrono::{Duration, Utc};
use oauth2::CsrfToken;
use serde_json::{Map, Value};

use automate_api::{ConnectionId, ConnectionStatus};

use super::{
    Connection, Integration, IntegrationContext, IntegrationInfo, SetupComplete, SetupRedirect,
};
use crate::config::{Config, TodoistAppConfig};
use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::prelude::*;
use crate::publishers::TODOIST_PROVIDER;

/// Where the visitor is sent to approve the request.
const AUTHORIZE_URL: &str = "https://app.todoist.com/oauth/authorize";

/// The API host, which the authorisation endpoint is deliberately not on.
const API_URL: &str = "https://api.todoist.com";

/// Where the authorisation code, and later the refresh token, is exchanged.
fn token_url(app: &TodoistAppConfig) -> String {
    format!(
        "{}/oauth/access_token",
        app.api_url.as_deref().unwrap_or(API_URL)
    )
}

/// The endpoint naming the account a grant belongs to.
fn user_url(app: &TodoistAppConfig) -> String {
    format!("{}/api/v1/user", app.api_url.as_deref().unwrap_or(API_URL))
}

/// The metadata key holding the email address of the linked Todoist account.
///
/// Not a credential, and the thing somebody actually recognises a connection
/// by, which is exactly the shape [`crate::connections::Connection::metadata`]
/// is for.
pub const ACCOUNT_EMAIL: &str = "email";

/// How long before expiry a token is renewed.
///
/// Todoist's access tokens last an hour, and a job that starts just inside the
/// window would otherwise make its first call with a token that expires
/// mid-run.
const RENEW_BEFORE: Duration = Duration::minutes(5);

pub struct TodoistAppIntegration;

crate::register_integration!(TodoistAppIntegration);

/// The configured application, if this instance has one.
pub fn app(config: &Config) -> Option<&TodoistAppConfig> {
    config.connections.todoist.app.as_ref()
}

fn require_app(config: &Config) -> Result<&TodoistAppConfig, human_errors::Error> {
    app(config).ok_or_else(|| {
        human_errors::user(
            "No Todoist application is configured on this Automate instance.",
            &["Add a [connections.todoist.app] section to your configuration."],
        )
    })
}

/// What Todoist returns from the token endpoint.
///
/// `refresh_token` and `expires_in` are optional because an application created
/// before Todoist introduced refresh tokens is issued a long-lived access token
/// and neither field.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// The subset of the user object that says whose account this is.
#[derive(Debug, Deserialize)]
struct TodoistUser {
    id: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    full_name: Option<String>,
}

impl TokenResponse {
    fn into_secret(self) -> ConnectionSecret {
        ConnectionSecret::OAuth2 {
            access_token: self.access_token,
            refresh_token: self.refresh_token.unwrap_or_default(),
            // A legacy application's token does not expire; Todoist says so by
            // returning a ten-year `expires_in`, so the arithmetic below says
            // the same thing without a second representation for it.
            expires_at: Utc::now() + Duration::seconds(self.expires_in.unwrap_or(315_360_000)),
        }
    }
}

/// The token endpoint's answer.
enum Grant {
    Issued(TokenResponse),

    /// Todoist will not honour this authorisation again — the refresh token has
    /// been used, revoked, or the app's credentials no longer match. Only the
    /// person who granted it can fix that, which is why it is a distinct answer
    /// rather than an error: a run that retries it on every schedule would fail
    /// forever with nothing to say why.
    Rejected(String),
}

/// Posts a form to Todoist's token endpoint and reads the grant back.
async fn exchange(
    http: &reqwest::Client,
    app: &TodoistAppConfig,
    form: &[(&str, &str)],
) -> Result<Grant, human_errors::Error> {
    let response = http
        .post(token_url(app))
        .form(form)
        .send()
        .await
        .wrap_user_err(
            "Failed to exchange the Todoist authorization for an access token.",
            &[
                "Check that your Todoist client ID and secret are correct.",
                "Check your network connection.",
            ],
        )?;

    if response.status().is_client_error() {
        // Todoist names the reason in an `error` field; a body that does not is
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
            "Todoist could not issue an access token.",
            &["This is usually temporary, and the operation will be retried."],
        )?
        .json()
        .await
        .wrap_system_err(
            "Todoist's token response was not in the format we expected.",
            &["This is likely a change on Todoist's side; please report it."],
        )
        .map(Grant::Issued)
}

/// Renews a grant that is about to expire, storing the rotated credential.
///
/// Returns the access token to use. A grant with no refresh token is a legacy
/// long-lived one and is returned as-is; there is nothing to renew it with, and
/// the alternative — refusing to use it — would break every account linked
/// before the application opted in.
///
/// Todoist rotates the refresh token on every use and revokes the whole grant
/// if a consumed one is replayed, so the new pair is written back before it is
/// used rather than after.
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
                "The connection '{}' does not hold a Todoist OAuth grant.",
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
        // Nothing to renew with. The stored token may still have life in it, so
        // it is used rather than turning a missing configuration into a run
        // that cannot happen.
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
                format!("Todoist will no longer accept this account's authorization ({reason})."),
                &["Reconnect your Todoist account from the connections page."],
            ));
        }
    };

    // Todoist omits the refresh token when it treats a call as a network retry
    // inside its grace window, and it cannot be recovered afterwards, so the one
    // we already hold is kept in that case.
    let secret = ConnectionSecret::OAuth2 {
        access_token: refreshed.access_token.clone(),
        refresh_token: refreshed.refresh_token.unwrap_or(refresh_token),
        expires_at: Utc::now() + Duration::seconds(refreshed.expires_in.unwrap_or(3600)),
    };

    store.update_secret(connection.id, secret).await?;

    debug!(connection.id = %connection.id, "Renewed the Todoist access token.");

    Ok(refreshed.access_token)
}

impl TodoistAppIntegration {
    /// Who the token belongs to, according to Todoist rather than to the
    /// request that carried the code.
    async fn identify(
        http: &reqwest::Client,
        app: &TodoistAppConfig,
        access_token: &str,
    ) -> Result<TodoistUser, human_errors::Error> {
        http.get(user_url(app))
            .bearer_auth(access_token)
            .send()
            .await
            .wrap_user_err(
                "Failed to read your Todoist account details.",
                &["Check your network connection and try connecting again."],
            )?
            .error_for_status()
            .wrap_user_err(
                "Todoist would not tell us which account this authorization belongs to.",
                &["Check that the app requests the data:read or data:read_write scope."],
            )?
            .json()
            .await
            .wrap_system_err(
                "Todoist's user response was not in the format we expected.",
                &["This is likely a change on Todoist's side; please report it."],
            )
    }
}

#[async_trait::async_trait]
impl Integration for TodoistAppIntegration {
    fn instances(&self, config: &Config) -> Vec<IntegrationInfo> {
        app(config)
            .map(|_| IntegrationInfo {
                id: TODOIST_PROVIDER.to_string(),
                name: "Todoist".to_string(),
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
            "{AUTHORIZE_URL}?client_id={}&scope={}&state={}&response_type=code&redirect_uri={}",
            urlencoding::encode(&app.client_id),
            urlencoding::encode(&app.scopes.join(",")),
            urlencoding::encode(&state),
            urlencoding::encode(&ctx.callback_url(self, id)),
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
                format!("Todoist did not complete the authorization: {error}."),
                &["Start connecting your Todoist account again."],
            ));
        }

        let code = query.get("code").ok_or_else(|| {
            human_errors::user(
                "Todoist did not return an authorization code.",
                &["Start connecting your Todoist account again."],
            )
        })?;

        let state = query.get("state").ok_or_else(|| {
            human_errors::user(
                "Todoist did not return the state we sent it.",
                &["Start connecting your Todoist account again from this application."],
            )
        })?;

        // Whose authorisation this is comes from what we recorded when the flow
        // began, never from the request: a callback arrives from Todoist, so
        // nothing in it can be trusted to name an account here.
        let owner = ctx.pending().claim(state, id).await?.tenant;

        let http = ctx.services().http_client();
        let token = match exchange(
            &http,
            &app,
            &[
                ("client_id", app.client_id.as_str()),
                ("client_secret", app.client_secret.as_str()),
                ("code", code.as_str()),
                ("redirect_uri", ctx.callback_url(self, id).as_str()),
            ],
        )
        .await?
        {
            Grant::Issued(token) => token,
            Grant::Rejected(reason) => {
                return Err(human_errors::user(
                    format!("Todoist rejected the authorization ({reason})."),
                    &[
                        "Check that [connections.todoist.app] client_id and client_secret match the app in Todoist's App Management Console.",
                        "Check that this instance's callback URL is registered as a redirect URI on that app.",
                    ],
                ));
            }
        };

        let user = Self::identify(&http, &app, &token.access_token).await?;
        let name = user
            .full_name
            .clone()
            .or_else(|| user.email.clone())
            .unwrap_or_else(|| "Todoist".to_string());

        let mut metadata = Map::new();
        if let Some(email) = user.email.clone() {
            metadata.insert(ACCOUNT_EMAIL.to_string(), Value::String(email));
        }

        let connections = ConnectionStore::new(ctx.for_tenant(owner.clone()), owner);

        // Keyed on the Todoist user id rather than the email, because that is
        // what a webhook delivery names and what stays put when somebody
        // changes their address.
        match connections
            .find_by_account(TODOIST_PROVIDER, &user.id)
            .await?
        {
            Some(existing) => {
                connections
                    .update_secret(existing.id, token.into_secret())
                    .await?;
                connections.update_metadata(existing.id, metadata).await?;
                info!(connection.id = %existing.id, "Refreshed an existing Todoist connection.");
            }
            None => {
                let created = connections
                    .create_with_metadata(
                        TODOIST_PROVIDER,
                        name,
                        Some(user.id.clone()),
                        token.into_secret(),
                        metadata,
                    )
                    .await?;
                info!(connection.id = %created.id, "Linked a Todoist account.");
            }
        }

        Ok(SetupComplete {
            heading: "Todoist connected".to_string(),
            message: "Your Todoist account is connected, you can close this window.".to_string(),
        })
    }

    async fn connections(
        &self,
        _id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<Vec<Connection>, human_errors::Error> {
        let store = ConnectionStore::new(ctx.services(), ctx.initiator.clone());

        Ok(store
            .list_for_provider(TODOIST_PROVIDER)
            .await?
            .into_iter()
            .map(|connection| {
                // The token itself never leaves the agent; the address it was
                // linked with is what somebody recognises the account by.
                let mut summary = Connection::new(connection.id.to_string(), connection.name)
                    .with_kind(connection.kind.as_str().to_string());

                if let Some(Value::String(email)) = connection.metadata.get(ACCOUNT_EMAIL) {
                    summary = summary.with_detail(email.clone());
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
            Some(existing) if existing.provider == TODOIST_PROVIDER => {
                store.delete(connection_id).await?;
                info!("Disconnected the Todoist account '{connection}'.");
                Ok(())
            }
            _ => Err(human_errors::user(
                format!("There is no Todoist connection named '{connection}'."),
                &["It may already have been removed."],
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TodoistAppConfig;
    use crate::services::AppContext;

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    async fn context() -> AppContext {
        context_at(None).await
    }

    /// A context whose Todoist calls are addressed at `api_url`.
    async fn context_at(api_url: Option<String>) -> AppContext {
        AppContext::new_mock(move |config| {
            config.connections.todoist.app = Some(TodoistAppConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                webhook_secret: None,
                scopes: vec!["data:read_write".to_string()],
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

    async fn link(context: &AppContext, account: &str) -> ConnectionId {
        let store = ConnectionStore::new(context.tenant(alice()), alice());
        let mut metadata = Map::new();
        metadata.insert(
            ACCOUNT_EMAIL.to_string(),
            Value::String(format!("{account}@example.com")),
        );

        store
            .create_with_metadata(
                TODOIST_PROVIDER,
                "Alice",
                Some(account.to_string()),
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
    /// otherwise the wizard would send people to a Todoist page that refuses
    /// them.
    #[tokio::test]
    async fn the_integration_is_only_listed_when_an_app_is_configured() {
        let configured = context().await;
        assert_eq!(
            TodoistAppIntegration.instances(&configured.config()).len(),
            1
        );

        let bare = AppContext::new_mock(|_| {}).await.unwrap();
        assert!(TodoistAppIntegration.instances(&bare.config()).is_empty());
    }

    /// The credential must never leave the agent; the address it was linked with
    /// is what somebody recognises the account by.
    #[tokio::test]
    async fn a_connection_never_carries_the_credential() {
        let context = context().await;
        link(&context, "2671355").await;

        let connections = TodoistAppIntegration
            .connections(TODOIST_PROVIDER, ctx(&context, alice()))
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
        assert!(serialized.contains("2671355@example.com"));
    }

    /// A grant that is still good is used as it stands, so an ordinary run does
    /// not spend a token exchange it does not need — and, more importantly, does
    /// not rotate a refresh token Todoist would then treat as replayed.
    #[tokio::test]
    async fn a_live_token_is_used_without_being_renewed() {
        let context = context().await;
        let id = link(&context, "2671355").await;

        let services = context.tenant(alice());
        let store = ConnectionStore::new(services.clone(), alice());
        let connection = store.get(id).await.unwrap().unwrap();
        let secret = store.open(&connection).unwrap();

        assert_eq!(
            access_token(&services, &connection, secret).await.unwrap(),
            "access-tYqR9"
        );
    }

    /// A legacy grant has no refresh token, and refusing to use it would break
    /// every account linked before the app opted in to refreshing.
    #[tokio::test]
    async fn a_grant_without_a_refresh_token_is_used_as_it_stands() {
        let context = context().await;
        let services = context.tenant(alice());
        let store = ConnectionStore::new(services.clone(), alice());

        let connection = store
            .create(
                TODOIST_PROVIDER,
                "Alice",
                Some("2671355".into()),
                ConnectionSecret::OAuth2 {
                    access_token: "legacy-token".into(),
                    refresh_token: String::new(),
                    expires_at: Utc::now() - Duration::days(1),
                },
            )
            .await
            .unwrap();

        let secret = store.open(&connection).unwrap();

        assert_eq!(
            access_token(&services, &connection, secret).await.unwrap(),
            "legacy-token"
        );
    }

    /// A grant is renewed before it lapses, and the rotated pair is stored.
    /// Todoist revokes the whole authorization if a consumed refresh token is
    /// presented again, so losing the new one costs the account.
    #[tokio::test]
    async fn an_expiring_grant_is_renewed_and_the_rotated_pair_is_stored() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let todoist = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/access_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-2",
                "refresh_token": "refresh-2",
                "expires_in": 3600,
                "token_type": "Bearer",
            })))
            .mount(&todoist)
            .await;

        let context = context_at(Some(todoist.uri())).await;
        let services = context.tenant(alice());
        let store = ConnectionStore::new(services.clone(), alice());

        let connection = store
            .create(
                TODOIST_PROVIDER,
                "Alice",
                Some("2671355".into()),
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
        assert!(matches!(
            store.open(&stored).unwrap(),
            ConnectionSecret::OAuth2 { access_token, refresh_token, .. }
                if access_token == "access-2" && refresh_token == "refresh-2"
        ));
    }

    /// An authorization Todoist will not honour again is marked, so the
    /// connections page offers Reconnect rather than leaving somebody to work
    /// out from a failing workflow that their grant is what is wrong.
    #[tokio::test]
    async fn a_dead_grant_marks_the_connection_as_needing_reconnection() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let todoist = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/access_token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "invalid_grant" })),
            )
            .mount(&todoist)
            .await;

        let context = context_at(Some(todoist.uri())).await;
        let services = context.tenant(alice());
        let store = ConnectionStore::new(services.clone(), alice());

        let connection = store
            .create(
                TODOIST_PROVIDER,
                "Alice",
                Some("2671355".into()),
                ConnectionSecret::OAuth2 {
                    access_token: "access-1".into(),
                    refresh_token: "refresh-1".into(),
                    expires_at: Utc::now() - Duration::hours(1),
                },
            )
            .await
            .unwrap();
        let secret = store.open(&connection).unwrap();

        let err = access_token(&services, &connection, secret)
            .await
            .expect_err("a dead grant cannot produce a token");
        assert!(err.is(human_errors::Kind::User), "{err}");

        assert_eq!(
            store.get(connection.id).await.unwrap().unwrap().status,
            ConnectionStatus::NeedsReauthorization,
        );
    }
}
