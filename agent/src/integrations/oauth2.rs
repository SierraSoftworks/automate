use std::collections::HashMap;

use super::{
    Connection, Integration, IntegrationContext, IntegrationInfo, SetupComplete, SetupRedirect,
};
use crate::config::Config;
use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::prelude::*;

pub struct OAuth2Integration;

crate::register_integration!(OAuth2Integration);

#[async_trait::async_trait]
impl Integration for OAuth2Integration {
    fn instances(&self, config: &Config) -> Vec<IntegrationInfo> {
        config
            .oauth2
            .iter()
            .map(|(id, provider)| IntegrationInfo {
                id: id.clone(),
                name: provider.name.clone(),
            })
            .collect()
    }

    fn acl(&self, config: &Config, id: &str) -> Option<Filter> {
        config
            .oauth2
            .get(id)
            .and_then(|provider| provider.acl.clone())
    }

    /// Kept at the original path: the redirect URI is registered with the
    /// provider, so moving it would require reconfiguring every provider's
    /// application.
    fn callback_path(&self, id: &str) -> String {
        format!("/oauth/{id}/callback")
    }

    async fn begin_setup(
        &self,
        id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<SetupRedirect, human_errors::Error> {
        let services = ctx.services();
        let provider = services.config().get_oauth2(id)?;
        let (url, state) = provider.get_login_url(ctx.callback_url(self, id))?;

        // Recorded before the visitor leaves, so the callback can establish
        // whose authorisation it is completing without trusting anything the
        // request tells it.
        ctx.pending()
            .begin(&state, ctx.initiator.clone(), id)
            .await?;

        Ok(SetupRedirect {
            url: url.to_string(),
            state,
        })
    }

    async fn complete_setup(
        &self,
        id: &str,
        ctx: IntegrationContext<'_>,
        query: &HashMap<String, String>,
    ) -> Result<SetupComplete, human_errors::Error> {
        let provider = ctx.context.config().get_oauth2(id)?;

        let code = query.get("code").ok_or_else(|| {
            human_errors::user(
                "The provider did not return an authorization code.",
                &["Start the setup again from the beginning."],
            )
        })?;

        let state = query.get("state").ok_or_else(|| {
            human_errors::user(
                "The provider did not return the state we sent it.",
                &["Start connecting the service again from this application."],
            )
        })?;

        // Whose authorisation this is comes from what we recorded when the flow
        // began, never from the request: a callback arrives from the provider,
        // so nothing in it can be trusted to name an account.
        let owner = ctx.pending().claim(state, id).await?.tenant;

        let token = provider
            .handle_callback(
                ctx.callback_url(self, id),
                code.clone(),
                &ctx.services().http_client(),
            )
            .await?;

        let connections = ConnectionStore::new(ctx.for_tenant(owner.clone()), owner.clone());
        let secret = ConnectionSecret::OAuth2 {
            access_token: token.access_token().to_string(),
            refresh_token: token.refresh_token().to_string(),
            expires_at: token.expires_at(),
        };

        // Re-authorising an account we already hold refreshes it in place, so
        // somebody reconnecting after an expiry is not left with two entries
        // they cannot tell apart.
        let connection = match connections.find_by_account(id, owner.as_str()).await? {
            Some(existing) => {
                let refreshed = connections.update_secret(existing.id, secret).await?;
                info!(
                    connection.id = %existing.id,
                    oauth.provider = id,
                    "Refreshed an existing connection after re-authorisation."
                );
                refreshed.unwrap_or(existing)
            }
            None => {
                let created = connections
                    .create(id, provider.name.clone(), Some(owner.to_string()), secret)
                    .await?;
                info!(
                    connection.id = %created.id,
                    oauth.provider = id,
                    "Linked a new account."
                );
                created
            }
        };

        // Start whatever this provider drives. Keyed by the connection, so
        // re-authorising an account that is already running does not leave two
        // schedules chasing each other.
        start_provider_workflows(id, connection.id, &ctx.for_tenant(owner)).await?;

        Ok(SetupComplete {
            heading: "Login complete".to_string(),
            message: format!(
                "You have successfully completed setting up {}, you can close this window.",
                provider.name
            ),
        })
    }

    /// The accounts linked to this provider by whoever is asking.
    async fn connections(
        &self,
        id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<Vec<Connection>, human_errors::Error> {
        let store = ConnectionStore::new(ctx.services(), ctx.initiator.clone());

        Ok(store
            .list_for_provider(id)
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

    /// Removes the stored credential, which is what stops this provider's
    /// workflows running for that account.
    async fn disconnect(
        &self,
        id: &str,
        connection: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<(), human_errors::Error> {
        let store = ConnectionStore::new(ctx.services(), ctx.initiator.clone());

        let connection_id = connection.parse().map_err(|err| {
            human_errors::user(
                format!("'{connection}' is not a connection identifier. {err}"),
                &["Use the identifier shown against the connection you want to remove."],
            )
        })?;

        // Checking the provider stops a connection being removed through another
        // integration's endpoint, which would otherwise let the wrong wizard
        // delete it.
        match store.get(connection_id).await? {
            Some(existing) if existing.provider == id => {
                store.delete(connection_id).await?;
                info!("Disconnected '{connection}' from the '{id}' integration.");
            }
            _ => {
                return Err(human_errors::user(
                    format!("There is no '{id}' connection named '{connection}'."),
                    &["It may already have been removed."],
                ));
            }
        }

        Ok(())
    }
}

/// Kicks off the workflows a newly linked account drives.
///
/// The set is keyed off the provider rather than configured, because a provider
/// and the work it enables are decided together in code; making it configurable
/// would only invite the two to disagree.
async fn start_provider_workflows(
    provider: &str,
    connection: automate_api::ConnectionId,
    services: &crate::services::AppServices,
) -> Result<(), human_errors::Error> {
    if provider == "spotify" {
        crate::jobs::SpotifyYearlyPlaylistWorkflow::dispatch(
            crate::jobs::SpotifyYearlyPlaylistTask { connection },
            Some(connection.to_string().into()),
            services,
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::AppContext;
    use crate::web::OAuth2Config;
    use automate_api::ConnectionId;

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    async fn context() -> AppContext {
        AppContext::new_mock(|config| {
            config.oauth2.insert(
                "spotify".to_string(),
                OAuth2Config {
                    name: "Spotify".to_string(),
                    deprecated_jobs: Vec::new(),
                    acl: None,
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

    fn ctx(context: &AppContext, initiator: TenantId) -> IntegrationContext<'_> {
        IntegrationContext {
            context,
            initiator,
            base_url: "https://automate.example.com",
        }
    }

    /// Links an account directly, standing in for a completed authorisation.
    async fn connect(context: &AppContext, tenant: TenantId, account: &str) -> ConnectionId {
        let store = ConnectionStore::new(context.tenant(tenant.clone()), tenant);

        store
            .create(
                "spotify",
                "Spotify",
                Some(account.to_string()),
                ConnectionSecret::OAuth2 {
                    access_token: "access-tYqR9".into(),
                    refresh_token: "refresh-Kx3Lm".into(),
                    expires_at: "2030-01-01T00:00:00Z".parse().unwrap(),
                },
            )
            .await
            .unwrap()
            .id
    }

    /// Each linked account is its own connection. Collapsing them into a single
    /// "connected" entry would leave a second account invisible, and give
    /// `disconnect` nothing to address.
    #[tokio::test]
    async fn every_linked_account_is_reported_as_its_own_connection() {
        let context = context().await;
        connect(&context, alice(), "alice-personal").await;
        connect(&context, alice(), "alice-work").await;

        let connections = OAuth2Integration
            .connections("spotify", ctx(&context, alice()))
            .await
            .unwrap();

        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0].kind.as_deref(), Some("oauth2"));
    }

    /// The tokens themselves must never leave the agent; only when they next need
    /// renewing is useful to show.
    #[tokio::test]
    async fn a_connection_never_carries_the_credential() {
        let context = context().await;
        connect(&context, alice(), "alice-personal").await;

        let connections = OAuth2Integration
            .connections("spotify", ctx(&context, alice()))
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
        assert!(serialized.contains("Renews 2030-01-01"));
    }

    #[tokio::test]
    async fn an_account_with_nothing_linked_has_nothing_to_report() {
        let context = context().await;

        assert!(
            OAuth2Integration
                .connections("spotify", ctx(&context, alice()))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn one_accounts_connections_are_invisible_to_another() {
        let context = context().await;
        let bob = TenantId::new("bob").unwrap();

        connect(&context, alice(), "alice-personal").await;

        assert!(
            OAuth2Integration
                .connections("spotify", ctx(&context, bob))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn disconnecting_removes_the_stored_credential() {
        let context = context().await;
        let id = connect(&context, alice(), "alice-personal").await;

        OAuth2Integration
            .disconnect("spotify", &id.to_string(), ctx(&context, alice()))
            .await
            .unwrap();

        assert!(
            OAuth2Integration
                .connections("spotify", ctx(&context, alice()))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn one_account_cannot_disconnect_anothers_credential() {
        let context = context().await;
        let bob = TenantId::new("bob").unwrap();
        let id = connect(&context, alice(), "alice-personal").await;

        assert!(
            OAuth2Integration
                .disconnect("spotify", &id.to_string(), ctx(&context, bob))
                .await
                .is_err()
        );

        assert_eq!(
            OAuth2Integration
                .connections("spotify", ctx(&context, alice()))
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_connection_cannot_be_removed_through_another_providers_wizard() {
        // Otherwise the wrong wizard could delete a credential it has no
        // business knowing about.
        let context = context().await;
        let id = connect(&context, alice(), "alice-personal").await;

        assert!(
            OAuth2Integration
                .disconnect("todoist", &id.to_string(), ctx(&context, alice()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn beginning_setup_records_who_started_it() {
        // The callback arrives from the provider, so this record is the only
        // trustworthy statement of whose authorisation it completes.
        let context = context().await;

        let redirect = OAuth2Integration
            .begin_setup("spotify", ctx(&context, alice()))
            .await
            .unwrap();

        let pending = ctx(&context, alice())
            .pending()
            .claim(&redirect.state, "spotify")
            .await
            .unwrap();

        assert_eq!(pending.tenant, alice());
        assert_eq!(pending.integration, "spotify");
    }

    #[tokio::test]
    async fn a_callback_without_a_recognised_state_is_refused() {
        // Reaching the provider's token endpoint before establishing whose
        // authorisation this is would let an unsolicited callback mint a
        // credential against an account of the attacker's choosing.
        let context = context().await;

        let outcome = OAuth2Integration
            .complete_setup(
                "spotify",
                ctx(&context, alice()),
                &HashMap::from([
                    ("code".to_string(), "abc".to_string()),
                    ("state".to_string(), "never-issued".to_string()),
                ]),
            )
            .await;

        let Err(err) = outcome else {
            panic!("an unrecognised state should be refused");
        };
        assert!(err.to_string().contains("could not match"), "{err}");
    }

    #[tokio::test]
    async fn a_callback_without_a_state_is_refused() {
        let context = context().await;

        assert!(
            OAuth2Integration
                .complete_setup(
                    "spotify",
                    ctx(&context, alice()),
                    &HashMap::from([("code".to_string(), "abc".to_string())]),
                )
                .await
                .is_err()
        );
    }
}
