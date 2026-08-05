use std::collections::HashMap;

use oauth2::CsrfToken;
use serde_json::{Map, Value};

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

/// The kv partition which used to record which accounts had installed the App.
///
/// It exists only for [`import_installations_as_connections`] to drain. Nothing
/// writes to it any more: an installation is a connection, and the account type
/// that used to justify a second record now lives in that connection's metadata.
const LEGACY_INSTALLATIONS_PARTITION: &str = "github/installations";

/// The metadata key holding whether the account an App is installed on is a
/// `User` or an `Organization`.
///
/// GitHub tells us, it changes how the account behaves, and it is not a secret —
/// which is exactly the shape of thing [`crate::connections::Connection::metadata`]
/// is for.
pub const ACCOUNT_TYPE: &str = "account_type";

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

/// What GitHub told us about the account, in the form a connection keeps it.
fn account_metadata(installation: &GitHubInstallation) -> Map<String, Value> {
    Map::from_iter([(
        ACCOUNT_TYPE.to_string(),
        Value::String(installation.account_type.clone()),
    )])
}

/// Records an installation as the connection a workflow can be pointed at.
///
/// Keyed on the account rather than the installation id, because reinstalling an
/// App issues a fresh id for the same account and the newer one should replace
/// the old rather than accumulate beside it.
///
/// # Why there is only one record
///
/// There used to be two: a registry entry holding the facts GitHub reports —
/// the id, the account, whether it is a user or an organisation — and a
/// connection saying Automate was meant to use it. The split existed for one
/// reason, that a connection had nowhere to put the account type, and it cost
/// the usual price of holding one thing twice: the integrations page read one
/// copy, the connections page and every picker read the other, and nothing
/// stopped them disagreeing.
///
/// The account type now lives in the connection's metadata, so the connection
/// says everything the registry did and the registry is gone. The
/// `installation_id` inside the sealed credential is rewritten here every time
/// an installation is recorded, so a reinstall — a fresh id for the same account
/// — updates the connection in place rather than leaving it pointing at an id
/// GitHub has forgotten.
pub async fn record_installation(
    installation: &GitHubInstallation,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<(), human_errors::Error> {
    link_installation(installation, services).await?;

    Ok(())
}

/// Creates, or refreshes, the connection standing for an installation.
///
/// Matched on the account for the same reason the old registry was keyed on it:
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

        // GitHub restates the account type on every delivery, so this is also
        // how an account that has been converted from a user to an organisation
        // stops being described as the wrong thing.
        store
            .update_metadata(existing.id, account_metadata(installation))
            .await?;

        debug!(
            connection.id = %existing.id,
            "Refreshed the connection for the GitHub App installation on '{}'.",
            installation.account
        );

        return Ok(existing.id);
    }

    let created = store
        .create_with_metadata(
            GITHUB_PROVIDER,
            installation.account.clone(),
            Some(installation.account.clone()),
            secret,
            account_metadata(installation),
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

/// Forgets an account's installation.
///
/// The connection is the whole record, so removing it is all there is to do: an
/// installation GitHub has taken away cannot mint a token, and leaving the
/// connection behind would only offer somebody a choice which fails the moment
/// they pick it.
pub async fn forget_installation(
    account: &str,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<(), human_errors::Error> {
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

/// The connection standing for a particular installation, if this account holds
/// one.
///
/// The installation id is inside the sealed credential rather than on the
/// record, so this is a scan rather than a lookup. An account holds a handful of
/// GitHub connections, which is cheaper than keeping a second copy of the id
/// beside the credential for the two to disagree about.
async fn connection_for_installation<S: Services>(
    store: &ConnectionStore<S>,
    installation_id: u64,
) -> Result<Option<crate::connections::Connection>, human_errors::Error> {
    for connection in store.list_for_provider(GITHUB_PROVIDER).await? {
        if let Ok(ConnectionSecret::GitHubApp {
            installation_id: id,
        }) = store.open(&connection)
            && id == installation_id
        {
            return Ok(Some(connection));
        }
    }

    Ok(None)
}

/// Brings installations recorded before they were connections across.
///
/// An account which installed the App before connections existed has a registry
/// entry and no connection, so the picker on its GitHub workflow has nothing to
/// offer — and there is no wizard left for the person to run, because from
/// GitHub's point of view the App is already installed. They would be stuck. So
/// the old registry is walked once, at start-up, and the missing connections are
/// made.
///
/// # Why the old entries are removed
///
/// This is the last thing that reads that partition, so anything it still holds
/// has to end up on the connection before it goes — the account type in
/// particular, which is the fact the second record existed for. Once that is
/// carried across the old copy is deleted, because a stale duplicate nobody
/// reads is exactly the thing somebody eventually reads by mistake.
///
/// An account which already had a connection is not skipped outright for the
/// same reason: its connection predates metadata, so the account type is still
/// only in the old partition and has to be moved before the entry is dropped.
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
        .list::<GitHubInstallation>(LEGACY_INSTALLATIONS_PARTITION)
        .await?;

    let store = ConnectionStore::for_services(services);

    // Read once rather than per installation, so the cost of the walk does not
    // grow with the square of how many accounts somebody has linked.
    let already_linked: HashMap<String, ConnectionId> = store
        .list_for_provider(GITHUB_PROVIDER)
        .await?
        .into_iter()
        .filter_map(|connection| Some((connection.account?, connection.id)))
        .collect();

    let mut imported = 0;
    let mut failed = 0;

    for (key, installation) in installations {
        let account = installation.account.clone();

        // Grouped so that the old entry is only dropped once everything it held
        // has landed on a connection, and so a failure anywhere in that leaves
        // the entry to be retried on the next start.
        let outcome = async {
            let created = match already_linked.get(&account) {
                Some(&id) => {
                    store
                        .update_metadata(id, account_metadata(&installation))
                        .await?;
                    None
                }
                None => Some(link_installation(&installation, services).await?),
            };

            services
                .kv()
                .remove(LEGACY_INSTALLATIONS_PARTITION, key)
                .await?;

            Ok::<_, human_errors::Error>(created)
        }
        .await;

        match outcome {
            Ok(Some(id)) => {
                imported += 1;

                warn!(
                    connection.id = %id,
                    "Imported the GitHub App installation on '{account}' as a connection, \
                     so your GitHub workflows can be pointed at it."
                );
            }
            Ok(None) => {
                debug!(
                    "Carried the GitHub App installation on '{account}' onto the connection \
                     that already stood for it."
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

    /// Every installation GitHub reports as belonging to this App.
    #[cfg(not(test))]
    async fn reported_installations(
        ctx: &IntegrationContext<'_>,
    ) -> Result<Vec<GitHubInstallation>, human_errors::Error> {
        Self::client(ctx)?.installations().await
    }

    /// The installations a test has arranged for GitHub to report.
    ///
    /// Reaching the real listing needs the App's RSA private key, and this
    /// repository deliberately does not ship one — the same reason
    /// [`GitHubAppClient::new_for_test`] exists. What the callback's tests are
    /// about is decided either side of this call and not by it: which account a
    /// completed setup lands in, and whether the state that got it here was ever
    /// issued. The listing itself is covered against a mock server in
    /// [`crate::services::github_app`].
    ///
    /// The configuration check is kept, so a test still has to have an App
    /// configured for the callback to get this far.
    #[cfg(test)]
    async fn reported_installations(
        ctx: &IntegrationContext<'_>,
    ) -> Result<Vec<GitHubInstallation>, human_errors::Error> {
        let config = ctx.context.config();
        Self::config(&config)?;

        Ok(tests::reported_installations())
    }

    /// Turns the installation id the browser came back with into a fact.
    ///
    /// The query string is under the browser's control, so an id in it is a
    /// claim; only the App's own credentials can confirm that the installation
    /// exists and say which account it is on.
    async fn resolve_installation(
        ctx: &IntegrationContext<'_>,
        installation_id: u64,
    ) -> Result<GitHubInstallation, human_errors::Error> {
        Self::reported_installations(ctx)
            .await?
            .into_iter()
            .find(|installation| installation.id == installation_id)
            .ok_or_else(|| {
                human_errors::user(
                    "GitHub does not report that installation as belonging to this app.",
                    &["Start the installation again from the setup page."],
                )
            })
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
        id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<SetupRedirect, human_errors::Error> {
        let config = ctx.context.config();
        let app = Self::config(&config)?;

        let state = CsrfToken::new_random().secret().clone();

        // Recorded before the visitor leaves, and this is what makes the state
        // worth minting at all. It says which account the resulting installation
        // belongs to, so the callback need not trust the request to tell it, and
        // it means a state we never issued cannot be redeemed against anything.
        ctx.pending()
            .begin(&state, ctx.initiator.clone(), id)
            .await?;

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
        id: &str,
        ctx: IntegrationContext<'_>,
        query: &HashMap<String, String>,
    ) -> Result<SetupComplete, human_errors::Error> {
        let state = query.get("state").ok_or_else(|| {
            human_errors::user(
                "GitHub did not return the state we sent it.",
                &["Start connecting the service again from this application."],
            )
        })?;

        // Whose installation this is comes from what we recorded when the flow
        // began, never from `ctx.initiator`: GitHub sends the visitor back here,
        // so nothing about the request can be trusted to name an account.
        //
        // Claimed first, before the id is even read, so that a callback we did
        // not start has no effect at all — not a write, not a call to GitHub,
        // and not the reassuring page below. Claiming is one-shot, so this is
        // also what stops a completion being replayed.
        let owner = ctx.pending().claim(state, id).await?.tenant;

        // GitHub sends people back here after a cancelled install too. It costs
        // the state, which is right: there is no installation to complete, so
        // the person has to start again either way.
        let Some(installation_id) = query
            .get("installation_id")
            .and_then(|id| id.parse::<u64>().ok())
        else {
            return Ok(SetupComplete {
                heading: "Nothing installed".to_string(),
                message: "No installation was created. You can close this window and start again if that was not what you intended.".to_string(),
            });
        };

        let installation = Self::resolve_installation(&ctx, installation_id).await?;

        record_installation(&installation, &ctx.for_tenant(owner.clone())).await?;

        info!(
            user.account = %owner,
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
        let store = ConnectionStore::new(ctx.services(), ctx.initiator.clone());

        let mut connections = Vec::new();
        for connection in store.list_for_provider(GITHUB_PROVIDER).await? {
            // The id this listing is addressed by is the installation id, which
            // lives in the sealed credential. A connection whose credential will
            // not open cannot be uninstalled through here anyway, so it is
            // reported as broken on the connections page rather than offered
            // here as something to act on.
            let Ok(ConnectionSecret::GitHubApp { installation_id }) = store.open(&connection)
            else {
                warn!(
                    connection.id = %connection.id,
                    "Skipping a GitHub connection whose credential is not an App installation."
                );
                continue;
            };

            // Showing the connection's own name, rather than the account GitHub
            // reports, is what makes this page and the connections page agree
            // about a connection somebody has renamed.
            let mut entry = Connection::new(installation_id.to_string(), connection.name.clone());

            if let Some(Value::String(account_type)) = connection.metadata.get(ACCOUNT_TYPE) {
                entry = entry.with_kind(account_type.clone());
            }

            connections.push(entry);
        }

        Ok(connections)
    }

    /// Uninstalls the App from the account. GitHub answers by delivering an
    /// `installation.deleted` webhook, which is what removes the connection —
    /// so there is exactly one path that maintains it, whether the uninstall was
    /// driven from here or from GitHub's own settings page.
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
        let store = ConnectionStore::new(ctx.services(), ctx.initiator.clone());
        if let Some(existing) = connection_for_installation(&store, installation_id).await?
            && let Some(account) = existing.account.as_deref()
        {
            forget_installation(account, &ctx.services()).await?;
        }

        info!("Uninstalled GitHub App installation {installation_id}.");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use automate_api::ConnectionKind;

    use super::*;
    use crate::services::{AppContext, ServicesContainer};

    type TestServices = ServicesContainer<crate::db::TenantDb>;

    thread_local! {
        /// What GitHub is pretending to report for the App under test.
        ///
        /// A thread-local rather than a global because `#[tokio::test]` runs each
        /// test on its own thread with a current-thread runtime, so one test's
        /// arrangement can never be seen by another's.
        static REPORTED_INSTALLATIONS: RefCell<Vec<GitHubInstallation>> =
            const { RefCell::new(Vec::new()) };
    }

    /// Read by [`GitHubAppIntegration::reported_installations`] in test builds.
    pub(super) fn reported_installations() -> Vec<GitHubInstallation> {
        REPORTED_INSTALLATIONS.with(|reported| reported.borrow().clone())
    }

    /// Arranges for GitHub to report these installations to the setup callback.
    fn github_reports(installations: &[GitHubInstallation]) {
        REPORTED_INSTALLATIONS.with(|reported| *reported.borrow_mut() = installations.to_vec());
    }

    async fn services() -> TestServices {
        TestServices::new_mock().await.expect("build mock services")
    }

    /// A context with a GitHub App configured, which is the precondition for the
    /// setup wizard existing at all.
    async fn app_context() -> AppContext {
        AppContext::new_mock(|config| {
            config.connections.github.app = Some(
                toml::from_str(
                    r#"
                    app_id = "123456"
                    slug = "my-automate"
                    private_key = "-----BEGIN RSA PRIVATE KEY-----\nnot-a-real-key\n-----END RSA PRIVATE KEY-----"
                    "#,
                )
                .expect("the sample app should parse"),
            );
        })
        .await
        .expect("build a mock context")
    }

    fn ctx(context: &AppContext, initiator: TenantId) -> IntegrationContext<'_> {
        IntegrationContext {
            context,
            initiator,
            base_url: "https://automate.example.com",
        }
    }

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    fn mallory() -> TenantId {
        TenantId::new("mallory").unwrap()
    }

    fn installation(id: u64, account: &str) -> GitHubInstallation {
        GitHubInstallation {
            id,
            account: account.to_string(),
            account_type: "Organization".to_string(),
        }
    }

    /// The state an instance which upgraded across this change is actually in:
    /// an entry in the partition that no longer exists.
    async fn seed_legacy_registry(services: &TestServices, installation: &GitHubInstallation) {
        services
            .kv()
            .set(
                LEGACY_INSTALLATIONS_PARTITION,
                installation.account.clone(),
                installation.clone(),
            )
            .await
            .expect("seed the old installation registry");
    }

    async fn legacy_registry(services: &TestServices) -> Vec<(String, GitHubInstallation)> {
        services
            .kv()
            .list::<GitHubInstallation>(LEGACY_INSTALLATIONS_PARTITION)
            .await
            .expect("read the old installation registry")
    }

    async fn linked(services: &TestServices) -> Vec<crate::connections::Connection> {
        ConnectionStore::for_services(services)
            .list_for_provider(GITHUB_PROVIDER)
            .await
            .expect("list the linked GitHub accounts")
    }

    async fn linked_for(
        context: &AppContext,
        tenant: TenantId,
    ) -> Vec<crate::connections::Connection> {
        ConnectionStore::new(context.tenant(tenant.clone()), tenant)
            .list_for_provider(GITHUB_PROVIDER)
            .await
            .expect("list the linked GitHub accounts")
    }

    fn account_type(connection: &crate::connections::Connection) -> Option<&str> {
        connection
            .metadata
            .get(ACCOUNT_TYPE)
            .and_then(Value::as_str)
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

    /// The account type is the fact that used to justify a second record. It has
    /// to be on the connection, or collapsing the two would have lost it.
    #[tokio::test]
    async fn the_connection_records_whether_the_account_is_a_user_or_an_organisation() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the installation");

        assert_eq!(
            account_type(&linked(&services).await[0]),
            Some("Organization")
        );
    }

    /// GitHub restates the account type on every delivery, and an account can be
    /// converted from a user to an organisation, so a later delivery has to be
    /// able to correct what we recorded.
    #[tokio::test]
    async fn a_converted_account_stops_being_described_as_the_wrong_kind() {
        let services = services().await;

        record_installation(
            &GitHubInstallation {
                id: 42,
                account: "notheotherben".into(),
                account_type: "User".into(),
            },
            &services,
        )
        .await
        .expect("record the installation");

        record_installation(
            &GitHubInstallation {
                id: 42,
                account: "notheotherben".into(),
                account_type: "Organization".into(),
            },
            &services,
        )
        .await
        .expect("record the converted account");

        assert_eq!(
            account_type(&linked(&services).await[0]),
            Some("Organization")
        );
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

        seed_legacy_registry(&services, &installation(1, "notheotherben")).await;
        seed_legacy_registry(&services, &installation(2, "SierraSoftworks")).await;

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

    /// The account type is the one thing the old partition held that a
    /// connection could not, so an import that dropped it would be trading a
    /// duplicate for a loss.
    #[tokio::test]
    async fn the_import_carries_the_account_type_onto_the_connection() {
        let services = services().await;

        seed_legacy_registry(
            &services,
            &GitHubInstallation {
                id: 1,
                account: "notheotherben".into(),
                account_type: "User".into(),
            },
        )
        .await;

        import_installations_as_connections(&services)
            .await
            .expect("import the installations");

        assert_eq!(account_type(&linked(&services).await[0]), Some("User"));
    }

    /// An instance that upgraded in two steps already has the connection, so the
    /// account type is still only in the old partition. Skipping such an account
    /// outright would drop the fact on the floor when the partition goes.
    #[tokio::test]
    async fn the_import_carries_the_account_type_onto_a_connection_that_already_existed() {
        let services = services().await;
        let store = ConnectionStore::for_services(&services);

        // A connection as an agent from before metadata wrote it: no account
        // type on it at all.
        store
            .create(
                GITHUB_PROVIDER,
                "notheotherben",
                Some("notheotherben".into()),
                ConnectionSecret::GitHubApp { installation_id: 1 },
            )
            .await
            .expect("link the account the old way");

        seed_legacy_registry(
            &services,
            &GitHubInstallation {
                id: 1,
                account: "notheotherben".into(),
                account_type: "User".into(),
            },
        )
        .await;

        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import the installations"),
            0,
            "there was nothing new to link"
        );

        assert_eq!(linked(&services).await.len(), 1);
        assert_eq!(account_type(&linked(&services).await[0]), Some("User"));
        assert!(legacy_registry(&services).await.is_empty());
    }

    /// A duplicate nobody reads is the thing somebody eventually reads by
    /// mistake, so the old copy has to go once everything on it has landed.
    #[tokio::test]
    async fn the_import_empties_the_old_partition() {
        let services = services().await;

        seed_legacy_registry(&services, &installation(1, "notheotherben")).await;
        seed_legacy_registry(&services, &installation(2, "SierraSoftworks")).await;

        import_installations_as_connections(&services)
            .await
            .expect("import the installations");

        assert!(
            legacy_registry(&services).await.is_empty(),
            "the old registry must not outlive the import that drained it"
        );
    }

    #[tokio::test]
    async fn the_import_can_be_run_again_without_duplicating_anything() {
        let services = services().await;

        seed_legacy_registry(&services, &installation(1, "notheotherben")).await;

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
    /// from a backup taken before the marker was written would walk the old
    /// registry again, and must not end up with two connections to the same
    /// account.
    #[tokio::test]
    async fn the_import_makes_no_duplicates_when_its_marker_is_missing() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the installation");
        seed_legacy_registry(&services, &installation(42, "SierraSoftworks")).await;

        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import the installations"),
            0
        );
        assert_eq!(linked(&services).await.len(), 1);
    }

    /// Once the import has run, `record_installation` is the only thing that
    /// creates a GitHub connection. A start-up scan that kept reading the old
    /// registry would be a second path doing it, quietly second-guessing whatever
    /// the person has since done in the browser.
    #[tokio::test]
    async fn the_import_stops_reading_the_old_registry_once_it_has_run() {
        let services = services().await;

        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import with nothing to import"),
            0
        );

        seed_legacy_registry(&services, &installation(42, "SierraSoftworks")).await;

        assert_eq!(
            import_installations_as_connections(&services)
                .await
                .expect("import after the marker was written"),
            0
        );
        assert!(linked(&services).await.is_empty());
    }

    /// The whole point of collapsing the two records: the integrations page and
    /// the connections page cannot disagree, because there is only one thing for
    /// them to read. A rename made on one shows up on the other.
    #[tokio::test]
    async fn the_integrations_page_and_the_connections_page_describe_the_same_record() {
        let context = app_context().await;
        let store = ConnectionStore::new(context.tenant(alice()), alice());

        record_installation(
            &installation(42, "SierraSoftworks"),
            &context.tenant(alice()),
        )
        .await
        .expect("record the installation");

        let connection = linked_for(&context, alice()).await.remove(0);
        store
            .rename(connection.id, "Work")
            .await
            .expect("rename the connection");

        let listed = GitHubAppIntegration
            .connections(GITHUB_PROVIDER, ctx(&context, alice()))
            .await
            .expect("list the integration's connections");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "42", "the id is what disconnect addresses");
        assert_eq!(listed[0].name, "Work");
        assert_eq!(listed[0].kind.as_deref(), Some("Organization"));

        // And removing it through the connections page leaves the integrations
        // page with nothing, because it was never a second record.
        store
            .delete(connection.id)
            .await
            .expect("remove the connection");

        assert!(
            GitHubAppIntegration
                .connections(GITHUB_PROVIDER, ctx(&context, alice()))
                .await
                .expect("list the integration's connections")
                .is_empty()
        );
    }

    /// One account's installations are nobody else's business, and the listing is
    /// now scoped by whoever is asking rather than read from a shared partition.
    #[tokio::test]
    async fn one_accounts_installations_are_invisible_to_another() {
        let context = app_context().await;

        record_installation(
            &installation(42, "SierraSoftworks"),
            &context.tenant(alice()),
        )
        .await
        .expect("record the installation");

        assert!(
            GitHubAppIntegration
                .connections(GITHUB_PROVIDER, ctx(&context, mallory()))
                .await
                .expect("list the integration's connections")
                .is_empty()
        );
    }

    /// A connection left pointing at an installation GitHub has revoked would
    /// still be offered by the picker, and would fail the moment somebody chose
    /// it.
    #[tokio::test]
    async fn uninstalling_at_github_removes_the_connection() {
        let services = services().await;

        record_installation(&installation(42, "SierraSoftworks"), &services)
            .await
            .expect("record the installation");

        forget_installation("SierraSoftworks", &services)
            .await
            .expect("forget the installation");

        assert!(linked(&services).await.is_empty());
    }

    /// Removing a connection to something else must not reach into GitHub's.
    #[tokio::test]
    async fn removing_another_services_connection_leaves_the_github_one_alone() {
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

    #[tokio::test]
    async fn beginning_setup_records_who_started_it() {
        // GitHub sends the visitor back to us, so this record is the only
        // trustworthy statement of whose installation the callback completes.
        let context = app_context().await;

        let redirect = GitHubAppIntegration
            .begin_setup(GITHUB_PROVIDER, ctx(&context, alice()))
            .await
            .expect("begin the setup");

        assert!(
            redirect
                .url
                .contains(&urlencoding::encode(&redirect.state).to_string()),
            "the state has to reach GitHub to come back: {}",
            redirect.url
        );

        let pending = ctx(&context, alice())
            .pending()
            .claim(&redirect.state, GITHUB_PROVIDER)
            .await
            .expect("claim the state that was just issued");

        assert_eq!(pending.tenant, alice());
        assert_eq!(pending.integration, GITHUB_PROVIDER);
    }

    #[tokio::test]
    async fn a_completion_whose_state_was_never_issued_is_refused() {
        // Otherwise anybody who can reach the callback can assert that an
        // installation happened, without ever having started one.
        let context = app_context().await;
        github_reports(&[installation(42, "SierraSoftworks")]);

        let outcome = GitHubAppIntegration
            .complete_setup(
                GITHUB_PROVIDER,
                ctx(&context, alice()),
                &HashMap::from([
                    ("state".to_string(), "never-issued".to_string()),
                    ("installation_id".to_string(), "42".to_string()),
                ]),
            )
            .await;

        let Err(err) = outcome else {
            panic!("an unrecognised state should be refused");
        };
        assert!(err.to_string().contains("could not match"), "{err}");
        assert!(
            linked_for(&context, alice()).await.is_empty(),
            "a refused completion must not have written anything"
        );
    }

    #[tokio::test]
    async fn a_completion_whose_state_has_already_been_used_is_refused() {
        // A callback URL sits in browser history, in a proxy log, and in
        // whatever the visitor pasted it into. Replaying it must not re-link an
        // account somebody has since removed.
        let context = app_context().await;
        github_reports(&[installation(42, "SierraSoftworks")]);

        let redirect = GitHubAppIntegration
            .begin_setup(GITHUB_PROVIDER, ctx(&context, alice()))
            .await
            .expect("begin the setup");

        let query = HashMap::from([
            ("state".to_string(), redirect.state.clone()),
            ("installation_id".to_string(), "42".to_string()),
        ]);

        GitHubAppIntegration
            .complete_setup(GITHUB_PROVIDER, ctx(&context, alice()), &query)
            .await
            .expect("the first completion should be accepted");

        let store = ConnectionStore::new(context.tenant(alice()), alice());
        let connection = linked_for(&context, alice()).await.remove(0);
        store
            .delete(connection.id)
            .await
            .expect("remove the connection again");

        assert!(
            GitHubAppIntegration
                .complete_setup(GITHUB_PROVIDER, ctx(&context, alice()), &query)
                .await
                .is_err(),
            "a state is one-shot"
        );
        assert!(
            linked_for(&context, alice()).await.is_empty(),
            "the replay must not have brought the connection back"
        );
    }

    /// Defect B: the account a completed setup lands in comes from the record
    /// made when it began, not from the request. Otherwise whoever can reach the
    /// callback chooses whose account an installation — and the repository access
    /// that comes with it — is filed under.
    #[tokio::test]
    async fn a_completion_lands_the_installation_in_the_account_that_started_the_setup() {
        let context = app_context().await;
        github_reports(&[installation(42, "SierraSoftworks")]);

        // Alice starts the setup.
        let redirect = GitHubAppIntegration
            .begin_setup(GITHUB_PROVIDER, ctx(&context, alice()))
            .await
            .expect("begin the setup");

        // Mallory completes it, naming herself as the initiator.
        GitHubAppIntegration
            .complete_setup(
                GITHUB_PROVIDER,
                ctx(&context, mallory()),
                &HashMap::from([
                    ("state".to_string(), redirect.state),
                    ("installation_id".to_string(), "42".to_string()),
                ]),
            )
            .await
            .expect("complete the setup");

        let alices = linked_for(&context, alice()).await;
        assert_eq!(
            alices.len(),
            1,
            "the installation belongs to the account that started the setup"
        );
        assert_eq!(alices[0].account.as_deref(), Some("SierraSoftworks"));

        assert!(
            linked_for(&context, mallory()).await.is_empty(),
            "the request must not be able to name whose account an installation lands in"
        );
    }

    #[tokio::test]
    async fn the_refusal_for_an_unknown_state_and_a_used_state_are_indistinguishable() {
        // Probing the callback must not tell somebody whether a state they have
        // is one we ever issued.
        let context = app_context().await;
        github_reports(&[installation(42, "SierraSoftworks")]);

        let redirect = GitHubAppIntegration
            .begin_setup(GITHUB_PROVIDER, ctx(&context, alice()))
            .await
            .expect("begin the setup");

        let complete = async |state: &str| {
            GitHubAppIntegration
                .complete_setup(
                    GITHUB_PROVIDER,
                    ctx(&context, alice()),
                    &HashMap::from([
                        ("state".to_string(), state.to_string()),
                        ("installation_id".to_string(), "42".to_string()),
                    ]),
                )
                .await
        };

        complete(&redirect.state)
            .await
            .expect("the first completion should be accepted");

        let used = complete(&redirect.state)
            .await
            .expect_err("a used state should be refused");
        let never = complete("never-issued")
            .await
            .expect_err("an unknown state should be refused");

        assert_eq!(used.to_string(), never.to_string());
    }

    #[tokio::test]
    async fn a_completion_without_a_state_is_refused() {
        let context = app_context().await;
        github_reports(&[installation(42, "SierraSoftworks")]);

        assert!(
            GitHubAppIntegration
                .complete_setup(
                    GITHUB_PROVIDER,
                    ctx(&context, alice()),
                    &HashMap::from([("installation_id".to_string(), "42".to_string())]),
                )
                .await
                .is_err()
        );
        assert!(linked_for(&context, alice()).await.is_empty());
    }

    /// A cancelled install comes back here too, and gets a reassuring page. It
    /// still has to be a callback we started, or that page is something anybody
    /// can make the wizard say.
    #[tokio::test]
    async fn a_cancelled_installation_is_still_only_reported_for_a_setup_we_started() {
        let context = app_context().await;

        assert!(
            GitHubAppIntegration
                .complete_setup(
                    GITHUB_PROVIDER,
                    ctx(&context, alice()),
                    &HashMap::from([("state".to_string(), "never-issued".to_string())]),
                )
                .await
                .is_err()
        );

        let redirect = GitHubAppIntegration
            .begin_setup(GITHUB_PROVIDER, ctx(&context, alice()))
            .await
            .expect("begin the setup");

        let outcome = GitHubAppIntegration
            .complete_setup(
                GITHUB_PROVIDER,
                ctx(&context, alice()),
                &HashMap::from([("state".to_string(), redirect.state)]),
            )
            .await
            .expect("a cancelled install is not an error");

        assert_eq!(outcome.heading, "Nothing installed");
        assert!(linked_for(&context, alice()).await.is_empty());
    }

    /// The installation id in the query string is the browser's claim, not a
    /// fact, so one GitHub does not vouch for must not be recorded.
    #[tokio::test]
    async fn an_installation_github_does_not_report_is_refused() {
        let context = app_context().await;
        github_reports(&[installation(42, "SierraSoftworks")]);

        let redirect = GitHubAppIntegration
            .begin_setup(GITHUB_PROVIDER, ctx(&context, alice()))
            .await
            .expect("begin the setup");

        let err = GitHubAppIntegration
            .complete_setup(
                GITHUB_PROVIDER,
                ctx(&context, alice()),
                &HashMap::from([
                    ("state".to_string(), redirect.state),
                    ("installation_id".to_string(), "99".to_string()),
                ]),
            )
            .await
            .expect_err("an installation of somebody else's app should be refused");

        assert!(err.to_string().contains("does not report"), "{err}");
        assert!(linked_for(&context, alice()).await.is_empty());
    }
}
