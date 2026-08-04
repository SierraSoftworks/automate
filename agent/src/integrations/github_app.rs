use std::collections::HashMap;

use oauth2::CsrfToken;

use super::{
    Connection, Integration, IntegrationContext, IntegrationInfo, SetupComplete, SetupRedirect,
};
use crate::config::Config;
use crate::prelude::*;
use crate::services::{GitHubAppClient, GitHubInstallation};

/// The kv partition recording which accounts have installed the App, keyed on
/// the account's login.
pub const INSTALLATIONS_PARTITION: &str = "github/installations";

pub struct GitHubAppIntegration;

crate::register_integration!(GitHubAppIntegration);

/// Records an installation so the admin area can list which accounts are
/// connected.
///
/// Keyed on the account rather than the installation id, because reinstalling an
/// App issues a fresh id for the same account and the newer one should replace
/// the old rather than accumulate beside it.
pub async fn record_installation(
    installation: &GitHubInstallation,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<(), human_errors::Error> {
    services
        .kv()
        .set(
            INSTALLATIONS_PARTITION,
            installation.account.clone(),
            installation.clone(),
        )
        .await
}

pub async fn forget_installation(
    account: &str,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<(), human_errors::Error> {
    services
        .kv()
        .remove(INSTALLATIONS_PARTITION, account.to_string())
        .await
}

impl GitHubAppIntegration {
    fn config(config: &Config) -> Result<&crate::config::GitHubAppConfig, human_errors::Error> {
        config.connections.github.app.as_ref().ok_or_else(|| {
            human_errors::user(
                "No GitHub App is configured on this Automate instance.",
                &["Add a [connections.github.app] section to your configuration."],
            )
        })
    }

    fn client(ctx: &IntegrationContext<'_>) -> Result<GitHubAppClient, human_errors::Error> {
        let config = ctx.context.config();
        GitHubAppClient::new(Self::config(&config)?, ctx.services().http_client())
    }
}

#[async_trait::async_trait]
impl Integration for GitHubAppIntegration {
    fn instances(&self, config: &Config) -> Vec<IntegrationInfo> {
        config
            .connections
            .github
            .app
            .as_ref()
            .map(|_| {
                vec![IntegrationInfo {
                    id: "github".to_string(),
                    name: "GitHub".to_string(),
                }]
            })
            .unwrap_or_default()
    }

    fn acl(&self, config: &Config, _id: &str) -> Option<Filter> {
        config
            .connections
            .github
            .app
            .as_ref()
            .and_then(|app| app.acl.clone())
    }

    async fn begin_setup(
        &self,
        _id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<SetupRedirect, human_errors::Error> {
        let config = ctx.context.config();
        let app = Self::config(&config)?;

        let state = CsrfToken::new_random().secret().clone();

        Ok(SetupRedirect {
            url: format!(
                "https://github.com/apps/{}/installations/new?state={}",
                urlencoding::encode(&app.slug),
                urlencoding::encode(&state),
            ),
            state,
        })
    }

    async fn complete_setup(
        &self,
        _id: &str,
        ctx: IntegrationContext<'_>,
        query: &HashMap<String, String>,
    ) -> Result<SetupComplete, human_errors::Error> {
        // GitHub sends people back here after a cancelled install too.
        let Some(installation_id) = query
            .get("installation_id")
            .and_then(|id| id.parse::<u64>().ok())
        else {
            return Ok(SetupComplete {
                heading: "Nothing installed".to_string(),
                message: "No installation was created. You can close this window and start again if that was not what you intended.".to_string(),
            });
        };

        // Resolve the account through the App's own credentials rather than
        // trusting the query string, which the browser controls.
        let client = Self::client(&ctx)?;
        let installation = client
            .installations()
            .await?
            .into_iter()
            .find(|i| i.id == installation_id)
            .ok_or_else(|| {
                human_errors::user(
                    "GitHub does not report that installation as belonging to this app.",
                    &["Start the installation again from the setup page."],
                )
            })?;

        record_installation(&installation, &ctx.services()).await?;

        info!(
            "Recorded GitHub App installation {} for '{}'.",
            installation.id, installation.account
        );

        Ok(SetupComplete {
            heading: "Installation complete".to_string(),
            message: format!(
                "Automate is now watching {}'s repositories. You can close this window.",
                installation.account
            ),
        })
    }

    async fn connections(
        &self,
        _id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<Vec<Connection>, human_errors::Error> {
        Ok(ctx
            .services()
            .kv()
            .list::<GitHubInstallation>(INSTALLATIONS_PARTITION)
            .await?
            .into_iter()
            .map(|(_, installation)| {
                Connection::new(installation.id.to_string(), installation.account)
                    .with_kind(installation.account_type)
            })
            .collect())
    }

    /// Uninstalls the App from the account. GitHub answers by delivering an
    /// `installation.deleted` webhook, which is what removes the account from our
    /// registry — so there is exactly one path that maintains it, whether the
    /// uninstall was driven from here or from GitHub's own settings page.
    async fn disconnect(
        &self,
        _id: &str,
        connection: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<(), human_errors::Error> {
        let installation_id: u64 = connection.parse().map_err(|_| {
            human_errors::user(
                format!("'{connection}' is not a GitHub App installation id."),
                &["Use an id reported by the integration's connections listing."],
            )
        })?;

        Self::client(&ctx)?.uninstall(installation_id).await?;

        // GitHub does not deliver an `installation.deleted` event for an
        // installation it had already forgotten, so drop our own record when the
        // uninstall was a no-op. Doing it unconditionally is safe: the webhook
        // removes by account, and this removes the same entry by lookup.
        if let Some((account, _)) = ctx
            .services()
            .kv()
            .list::<GitHubInstallation>(INSTALLATIONS_PARTITION)
            .await?
            .into_iter()
            .find(|(_, installation)| installation.id == installation_id)
        {
            forget_installation(&account, &ctx.services()).await?;
        }

        info!("Uninstalled GitHub App installation {installation_id}.");

        Ok(())
    }
}
