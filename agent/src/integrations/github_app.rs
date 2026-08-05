use std::collections::{HashMap, HashSet};

use oauth2::CsrfToken;

use automate_api::ConnectionId;
use chrono::{DateTime, Utc};

use super::{
    Connection, Integration, IntegrationContext, IntegrationInfo, SetupComplete, SetupRedirect,
};
use crate::config::Config;
use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::prelude::*;
use crate::services::{GitHubAppClient, GitHubInstallation};
use crate::workflow_migration::MIGRATIONS_PARTITION;

/// The kv partition recording which accounts have installed the App, keyed on
/// the account's login.
pub const INSTALLATIONS_PARTITION: &str = "github/installations";

/// The provider GitHub App installations are linked under.
///
/// The same string as the id this integration reports its instance under, and as
/// the webhook source these deliveries arrive on, because they all name the same
/// thing: the GitHub account somebody installed the App on.
///
/// [`crate::webhooks::github`] declares a private constant of its own holding
/// this string, for the picker on its workflow. That copy predates installations
/// being connections at all and cannot be reached from here — the module is
/// private — so it should be deleted in favour of this one, which is the copy
/// the connection layer owns.
pub const GITHUB_PROVIDER: &str = "github";

/// The marker saying this tenant's pre-existing installations have been brought
/// across as connections.
pub const INSTALLATIONS_AS_CONNECTIONS: &str = "github-installations-as-connections";

pub struct GitHubAppIntegration;

crate::register_integration!(GitHubAppIntegration);

/// What was written down when the import happened.
///
/// Neither field is read; they are here so somebody looking at the database
/// afterwards can tell what ran and when, which a bare `true` would not.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportMarker {
    at: DateTime<Utc>,
    imported: usize,
}

/// Records an installation, both in the registry the admin area lists and as the
/// connection a workflow can be pointed at.
///
/// Keyed on the account rather than the installation id, because reinstalling an
/// App issues a fresh id for the same account and the newer one should replace
/// the old rather than accumulate beside it.
///
/// # Which of the two records is authoritative
///
/// [`INSTALLATIONS_PARTITION`] is authoritative for the *facts* about an
/// installation: its id, the account, and whether that account is a user or an
/// organisation. It is our mirror of what GitHub tells us, and a
/// [`crate::connections::Connection`] cannot replace it because there is nowhere
/// on one to put the account type.
///
/// The connection is authoritative for whether Automate is meant to *use* the
/// account. It is what a workflow stores a reference to, what the picker offers,
/// and what somebody removes when they want us to stop. The `installation_id` it
/// carries is a copy of the registry's, rewritten here every time an
/// installation is recorded, so a reinstall — a fresh id for the same account —
/// updates the connection in place rather than leaving it pointing at an id
/// GitHub has forgotten.
///
/// Both are written here and both are removed together, by
/// [`forget_installation`] and by [`ConnectionStore::delete`], so there is a
/// single path maintaining the pair and no way for one to outlive the other.
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
        .await?;

    link_installation(installation, services).await?;

    Ok(())
}

/// Creates, or refreshes, the connection standing for an installation.
///
/// Matched on the account for the same reason the registry is keyed on it:
/// somebody who reinstalls expects the account they linked to still be there,
/// rather than to acquire a second entry they cannot tell from the first.
async fn link_installation(
    installation: &GitHubInstallation,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<ConnectionId, human_errors::Error> {
    let store = ConnectionStore::for_services(services);
    let secret = ConnectionSecret::GitHubApp {
        installation_id: installation.id,
    };

    if let Some(existing) = store
        .find_by_account(GITHUB_PROVIDER, &installation.account)
        .await?
    {
        // Replacing the credential also clears the status, which is what we
        // want: an installation GitHub has just told us about works, whatever
        // the connection was doing the last time somebody used it.
        store.update_secret(existing.id, secret).await?;

        debug!(
            connection.id = %existing.id,
            "Refreshed the connection for the GitHub App installation on '{}'.",
            installation.account
        );

        return Ok(existing.id);
    }

    let created = store
        .create(
            GITHUB_PROVIDER,
            installation.account.clone(),
            Some(installation.account.clone()),
            secret,
        )
        .await?;

    info!(
        connection.id = %created.id,
        "Linked GitHub App installation {} on '{}' as a connection.",
        installation.id,
        installation.account
    );

    Ok(created.id)
}

/// Forgets an account's installation, in both of the places
/// [`record_installation`] wrote it.
///
/// The connection goes too: an installation GitHub has taken away cannot mint a
/// token, so leaving it behind would only offer somebody a choice which fails
/// the moment they pick it.
pub async fn forget_installation(
    account: &str,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<(), human_errors::Error> {
    services
        .kv()
        .remove(INSTALLATIONS_PARTITION, account.to_string())
        .await?;

    let store = ConnectionStore::for_services(services);
    if let Some(existing) = store.find_by_account(GITHUB_PROVIDER, account).await? {
        store.delete(existing.id).await?;

        info!(
            connection.id = %existing.id,
            "Removed the connection for the GitHub App installation on '{account}'."
        );
    }

    Ok(())
}

/// Brings installations recorded before they were connections across.
///
/// An account which installed the App before this existed has a registry entry
/// and no connection, so the picker on its GitHub workflow has nothing to offer
/// — and there is no wizard left for the person to run, because from GitHub's
/// point of view the App is already installed. They would be stuck. So the
/// registry is walked once, at start-up, and the missing connections are made.
///
/// # Why a marker as well as a per-installation check
///
/// The point of the marker is that the registry stops being a source of
/// connections once this has run. From then on [`record_installation`] is the
/// only thing that creates one, which is a single path somebody can follow; a
/// scan that kept reading the registry at every start would be a second one,
/// quietly second-guessing whatever the person has since done in the browser.
/// The per-installation check is kept beside the marker so that losing it — an
/// instance restored from a backup taken before it was written, say — cannot
/// produce duplicates.
///
/// # Why one failure does not stop the rest
///
/// An instance with several linked accounts should not lose all of them to
/// whichever one is broken, so a failure is logged against its account and the
/// walk continues. The marker is only written when every installation made it,
/// leaving a transient failure to be retried on the next start rather than
/// recorded as done.
#[instrument(
    "integrations.github_app.import_installations",
    skip(services),
    err(Display)
)]
pub async fn import_installations_as_connections(
    services: &(impl Services + Send + Sync + 'static),
) -> Result<usize, human_errors::Error> {
    if services
        .kv()
        .get::<ImportMarker>(MIGRATIONS_PARTITION, INSTALLATIONS_AS_CONNECTIONS)
        .await?
        .is_some()
    {
        return Ok(0);
    }

    let installations = services
        .kv()
        .list::<GitHubInstallation>(INSTALLATIONS_PARTITION)
        .await?;

    let store = ConnectionStore::for_services(services);

    // Read once rather than per installation, so the cost of the walk does not
    // grow with the square of how many accounts somebody has linked.
    let already_linked: HashSet<String> = store
        .list_for_provider(GITHUB_PROVIDER)
        .await?
        .into_iter()
        .filter_map(|connection| connection.account)
        .collect();

    let mut imported = 0;
    let mut failed = 0;

    for (_, installation) in installations {
        if already_linked.contains(&installation.account) {
            continue;
        }

        let account = &installation.account;

        match link_installation(&installation, services).await {
            Ok(id) => {
                imported += 1;

                warn!(
                    connection.id = %id,
                    "Imported the GitHub App installation on '{account}' as a connection, \
                     so your GitHub workflows can be pointed at it."
                );
            }
            Err(err) => {
                failed += 1;

                warn!(
                    error = %err,
                    "Failed to import the GitHub App installation on '{account}' as a connection; \
                     it will be retried the next time the agent starts."
                );
            }
        }
    }

    if failed == 0 {
        // Written even when there was nothing to import, so an installation that
        // has never used the GitHub App does not re-scan for one at every start
        // for the rest of its life.
        services
            .kv()
            .set(
                MIGRATIONS_PARTITION,
                INSTALLATIONS_AS_CONNECTIONS,
                ImportMarker {
                    at: Utc::now(),
                    imported,
                },
            )
            .await?;
    }

    Ok(imported)
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
                    id: GITHUB_PROVIDER.to_string(),
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

#[cfg(test)]
mod tests {
    use automate_api::ConnectionKind;

    use super::*;
    use crate::services::ServicesContainer;

    type TestServices = ServicesContainer<crate::db::TenantDb>;

    async fn services() -> TestServices {
        TestServices::new_mock().await.expect("build mock services")
    }

    fn installation(id: u64, account: &str) -> GitHubInstallation {
        GitHubInstallation {
            id,
            account: account.to_string(),
            account_type: "Organization".to_string(),
        }
    }

    /// The state an instance which upgraded across this change is actually in:
    /// a registry entry and no connection to go with it.
    async fn seed_registry(services: &TestServices, installation: &GitHubInstallation) {
        services
            .kv()
            .set(
                INSTALLATIONS_PARTITION,
                installation.account.clone(),
                installation.clone(),
            )
            .await
            .expect("seed the installation registry");
    }

    async fn linked(services: &TestServices) -> Vec<crate::connections::Connection> {
        ConnectionStore::for_services(services)
            .list_for_provider(GITHUB_PROVIDER)
            .await
            .expect("list the linked GitHub accounts")
    }

    async fn registered(services: &TestServices, account: &str) -> Option<GitHubInstallation> {
        services
            .kv()
            .get::<GitHubInstallation>(INSTALLATIONS_PARTITION, account.to_string())
            .await
            .expect("read the installation registry")
    }

    #[tokio::test]
    async fn recording_an_installation_links_the_account_as_a_connection() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the installation");

        let linked = linked(&services).await;

        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].kind, ConnectionKind::GitHubApp);
        // The account is what tells two GitHub connections apart, and the name is
        // what somebody picks from a list, so both have to say which account this
        // is rather than merely "GitHub".
        assert_eq!(linked[0].account.as_deref(), Some("SierraSoftworks"));
        assert_eq!(linked[0].name, "SierraSoftworks");
    }

    /// The installation id is the whole credential — it is what mints a token —
    /// so a connection which cannot produce one is decoration.
    #[tokio::test]
    async fn the_connection_carries_the_installation_id() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the installation");

        let store = ConnectionStore::for_services(&services);
        let linked = linked(&services).await;

        match store.open(&linked[0]).expect("open the credential") {
            ConnectionSecret::GitHubApp { installation_id } => assert_eq!(installation_id, 42),
            other => panic!("expected a GitHub App credential, got {other:?}"),
        }
    }

    /// GitHub re-delivers `installation` events — `new_permissions_accepted`
    /// after every permissions change, `unsuspend` after a suspension — so
    /// recording an account we already hold is the ordinary case, not the edge.
    #[tokio::test]
    async fn recording_the_same_installation_again_does_not_add_a_second_connection() {
        let services = services().await;

        for _ in 0..3 {
            record_installation(&installation(42, "SierraSoftworks"), &services)
                .await
                .expect("record the installation");
        }

        assert_eq!(linked(&services).await.len(), 1);
    }

    /// Reinstalling issues a fresh id for the same account. The connection a
    /// workflow already refers to has to follow it, or that workflow keeps asking
    /// for a token against an installation GitHub has forgotten.
    #[tokio::test]
    async fn reinstalling_moves_the_existing_connection_onto_the_new_installation() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the first installation");
        let before = linked(&services).await;

        record_installation(&installation(99, "SierraSoftworks"), &services)
            .await
            .expect("record the replacement installation");
        let after = linked(&services).await;

        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].id, before[0].id,
            "the identifier a workflow stores must survive a reinstall"
        );

        let store = ConnectionStore::for_services(&services);
        match store.open(&after[0]).expect("open the credential") {
            ConnectionSecret::GitHubApp { installation_id } => assert_eq!(installation_id, 99),
            other => panic!("expected a GitHub App credential, got {other:?}"),
        }
    }

    /// Somebody who installed the App before this existed cannot link it by hand:
    /// GitHub already considers it installed, so there is no wizard left to run.
    /// The import is the only way they get a connection.
    #[tokio::test]
    async fn the_import_brings_pre_existing_installations_across() {
        let services = services().await;

        seed_registry(&services, &installation(1, "notheotherben")).await;
        seed_registry(&services, &installation(2, "SierraSoftworks")).await;

        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import the installations"),
            2
        );

        let store = ConnectionStore::for_services(&services);
        let mut accounts: Vec<String> = linked(&services)
            .await
            .into_iter()
            .filter_map(|connection| connection.account)
            .collect();
        accounts.sort();
        assert_eq!(accounts, vec!["SierraSoftworks", "notheotherben"]);

        // The imported credentials have to be usable, not merely present.
        for connection in linked(&services).await {
            assert!(matches!(
                store.open(&connection).expect("open the credential"),
                ConnectionSecret::GitHubApp { .. }
            ));
        }
    }

    #[tokio::test]
    async fn the_import_can_be_run_again_without_duplicating_anything() {
        let services = services().await;

        seed_registry(&services, &installation(1, "notheotherben")).await;

        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import the installations"),
            1
        );
        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import the installations again"),
            0
        );
        assert_eq!(linked(&services).await.len(), 1);
    }

    /// Belt and braces for the case the marker cannot cover: an instance restored
    /// from a backup taken before the marker was written would walk the registry
    /// again, and must not end up with two connections to the same account.
    #[tokio::test]
    async fn the_import_makes_no_duplicates_when_its_marker_is_missing() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the installation");

        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import the installations"),
            0
        );
        assert_eq!(linked(&services).await.len(), 1);
    }

    /// Once the import has run, `record_installation` is the only thing that
    /// creates a GitHub connection. A start-up scan that kept reading the
    /// registry would be a second path doing it, quietly second-guessing whatever
    /// the person has since done in the browser.
    #[tokio::test]
    async fn the_import_stops_reading_the_registry_once_it_has_run() {
        let services = services().await;

        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import with nothing to import"),
            0
        );

        seed_registry(&services, &installation(42, "SierraSoftworks")).await;

        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import after the marker was written"),
            0
        );
        assert!(linked(&services).await.is_empty());
    }

    /// The pair is written together, so it has to go together. An integrations
    /// page still listing an account whose connection was just removed gives the
    /// person two answers and no way to tell which is right.
    #[tokio::test]
    async fn removing_the_connection_also_forgets_the_installation() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the installation");

        let store = ConnectionStore::for_services(&services);
        let connection = linked(&services).await.remove(0);
        assert!(store.delete(connection.id).await.expect("delete it"));

        assert!(linked(&services).await.is_empty());
        assert!(
            registered(&services, "SierraSoftworks").await.is_none(),
            "the registry must not keep listing an account whose connection has gone"
        );
    }

    /// Removing a connection to something else must not reach into GitHub's
    /// registry, which the provider check is the only thing standing between.
    #[tokio::test]
    async fn removing_another_services_connection_leaves_the_registry_alone() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the installation");

        let store = ConnectionStore::for_services(&services);
        let todoist = store
            .create(
                "todoist",
                "Personal",
                Some("SierraSoftworks".into()),
                ConnectionSecret::ApiKey { key: "tok".into() },
            )
            .await
            .expect("link a Todoist account");

        assert!(store.delete(todoist.id).await.expect("delete it"));

        assert_eq!(linked(&services).await.len(), 1);
        assert!(registered(&services, "SierraSoftworks").await.is_some());
    }

    /// The other direction of the same pairing. A connection left pointing at an
    /// installation GitHub has revoked would still be offered by the picker, and
    /// would fail the moment somebody chose it.
    #[tokio::test]
    async fn uninstalling_at_github_removes_the_connection_too() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the installation");

        forget_installation("SierraSoftworks", &services)
            .await
            .expect("forget the installation");

        assert!(linked(&services).await.is_empty());
        assert!(registered(&services, "SierraSoftworks").await.is_none());
    }

    /// Forgetting an account we never held is what happens when GitHub delivers
    /// `installation.deleted` twice, or for an App somebody removed before we
    /// ever saw it installed.
    #[tokio::test]
    async fn forgetting_an_account_we_never_held_is_not_an_error() {
        let services = services().await;

        forget_installation("SierraSoftworks", &services)
            .await
            .expect("forgetting an unknown account should be a no-op");
    }
}
