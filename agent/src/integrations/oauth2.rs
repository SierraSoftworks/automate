use std::collections::HashMap;

use super::{
    Connection, Integration, IntegrationContext, IntegrationInfo, SetupComplete, SetupRedirect,
};
use crate::config::Config;
use crate::prelude::*;
use crate::web::OAuth2RefreshToken;

/// How many connections we are willing to list for one provider. Far more than
/// anyone is likely to link, but bounded so a runaway queue cannot produce an
/// unbounded response.
const MAX_CONNECTIONS: usize = 100;

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
        let provider = ctx.services.config().get_oauth2(id)?;
        let (url, state) = provider.get_login_url(ctx.callback_url(self, id))?;

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
        let provider = ctx.services.config().get_oauth2(id)?;

        let code = query.get("code").ok_or_else(|| {
            human_errors::user(
                "The provider did not return an authorization code.",
                &["Start the setup again from the beginning."],
            )
        })?;

        let token = provider
            .handle_callback(
                ctx.callback_url(self, id),
                code.clone(),
                &ctx.services.http_client(),
            )
            .await?;

        // Deliberately enqueued without a key. We do not yet know whose account
        // this is, so anything we invented here would be meaningless; the job's
        // first run replaces it with an account-derived key (see
        // `JobContext::key`), which is also what collapses a repeat connection of
        // the same account onto the existing one.
        for partition in provider.jobs.iter().cloned() {
            ctx.services
                .queue()
                .enqueue(partition, token.clone(), None, None)
                .await?;
        }

        Ok(SetupComplete {
            heading: "Login complete".to_string(),
            message: format!(
                "You have successfully completed setting up {}, you can close this window.",
                provider.name
            ),
        })
    }

    /// An OAuth2 provider holds each connection's credential as a queued job
    /// payload rather than in a registry, so the queue is the register of who is
    /// connected.
    ///
    /// The provider's first job partition is authoritative: every partition is
    /// seeded from the same callback, so they start out in step. They can drift
    /// once the jobs re-key themselves, which is why [`Self::disconnect`] clears
    /// the key from all of them rather than only this one.
    async fn connections(
        &self,
        id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<Vec<Connection>, human_errors::Error> {
        let provider = ctx.services.config().get_oauth2(id)?;

        let Some(partition) = provider.jobs.first().cloned() else {
            return Ok(vec![]);
        };

        Ok(ctx
            .services
            .queue()
            .peek::<_, OAuth2RefreshToken>(partition, MAX_CONNECTIONS)
            .await?
            .into_iter()
            .map(|message| {
                // The token itself must never leave the agent; only when it next
                // needs renewing is useful to an administrator.
                Connection::new(message.key.clone(), message.key)
                    .with_kind(provider.name.clone())
                    .with_detail(format!(
                        "Renews {}",
                        message.payload.expires_at().format("%Y-%m-%d %H:%M UTC")
                    ))
            })
            .collect())
    }

    /// Drops the connection's credential, which is what stops the provider's
    /// jobs from running for that account.
    async fn disconnect(
        &self,
        id: &str,
        connection: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<(), human_errors::Error> {
        let provider = ctx.services.config().get_oauth2(id)?;

        for partition in provider.jobs.iter().cloned() {
            ctx.services
                .queue()
                .purge(partition, connection.to_string())
                .await?;
        }

        info!("Disconnected '{connection}' from the '{id}' integration.");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{AppServices, ServicesContainer};
    use crate::web::OAuth2Config;

    const PARTITION: &str = "spotify/yearly-playlist";

    async fn services() -> AppServices {
        ServicesContainer::new_custom_mock(|config, _| {
            config.oauth2.insert(
                "spotify".to_string(),
                OAuth2Config {
                    name: "Spotify".to_string(),
                    jobs: vec![PARTITION.to_string(), "spotify/other".to_string()],
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

    fn ctx(services: &AppServices) -> IntegrationContext<'_> {
        IntegrationContext {
            services,
            base_url: "https://automate.example.com",
        }
    }

    async fn connect(services: &AppServices, key: &str) {
        let token: OAuth2RefreshToken = serde_json::from_value(serde_json::json!({
            "access_token": "at",
            "refresh_token": "rt",
            "expires_at": "2030-01-01T00:00:00Z",
        }))
        .unwrap();

        for partition in [PARTITION, "spotify/other"] {
            services
                .queue()
                .enqueue(partition, token.clone(), Some(key.to_string().into()), None)
                .await
                .unwrap();
        }
    }

    /// Each queued credential is one connected account. Collapsing them into a
    /// single "connected" entry would leave a second account invisible, and give
    /// `disconnect` nothing to address.
    #[tokio::test]
    async fn every_queued_credential_is_reported_as_its_own_connection() {
        let services = services().await;
        connect(&services, "alice").await;
        connect(&services, "bob").await;

        let mut connections = OAuth2Integration
            .connections("spotify", ctx(&services))
            .await
            .unwrap();
        connections.sort_by(|a, b| a.id.cmp(&b.id));

        assert_eq!(
            connections
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            vec!["alice", "bob"]
        );
        assert_eq!(connections[0].kind.as_deref(), Some("Spotify"));
    }

    /// The tokens themselves must never leave the agent; only when they next need
    /// renewing is useful to an administrator.
    #[tokio::test]
    async fn a_connection_never_carries_the_credential() {
        let services = services().await;
        connect(&services, "alice").await;

        let connections = OAuth2Integration
            .connections("spotify", ctx(&services))
            .await
            .unwrap();

        let serialized = serde_json::to_string(&connections).unwrap();
        assert!(!serialized.contains("at"), "leaked an access token");
        assert!(!serialized.contains("rt"), "leaked a refresh token");
        assert!(serialized.contains("Renews 2030-01-01"));
    }

    #[tokio::test]
    async fn a_provider_with_no_jobs_has_nothing_to_report() {
        let services = ServicesContainer::new_custom_mock(|config, _| {
            config.oauth2.insert(
                "spotify".to_string(),
                OAuth2Config {
                    name: "Spotify".to_string(),
                    jobs: vec![],
                    acl: None,
                    client_id: "c".to_string(),
                    client_secret: "s".to_string(),
                    auth_url: "https://accounts.spotify.com/authorize".to_string(),
                    token_url: "https://accounts.spotify.com/api/token".to_string(),
                    scopes: vec![],
                    todoist: Default::default(),
                },
            );
        })
        .await
        .unwrap();

        assert!(
            OAuth2Integration
                .connections("spotify", ctx(&services))
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A credential is seeded into every one of the provider's partitions, so
    /// disconnecting has to clear all of them — leaving one behind would keep the
    /// account running under a job the operator thought they had revoked.
    #[tokio::test]
    async fn disconnecting_clears_the_credential_from_every_partition() {
        let services = services().await;
        connect(&services, "alice").await;
        connect(&services, "bob").await;

        OAuth2Integration
            .disconnect("spotify", "alice", ctx(&services))
            .await
            .unwrap();

        for partition in [PARTITION, "spotify/other"] {
            let remaining = services
                .queue()
                .peek::<_, serde_json::Value>(partition, 10)
                .await
                .unwrap();
            assert_eq!(
                remaining.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(),
                vec!["bob"],
                "'{partition}' should only have bob left"
            );
        }
    }

    /// Disconnecting something already gone is how a stale entry gets cleaned up,
    /// so it must not be an error.
    #[tokio::test]
    async fn disconnecting_an_unknown_connection_is_not_an_error() {
        let services = services().await;
        OAuth2Integration
            .disconnect("spotify", "nobody", ctx(&services))
            .await
            .unwrap();
    }
}
