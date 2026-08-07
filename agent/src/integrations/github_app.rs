use std::collections::HashMap;

use oauth2::CsrfToken;
use serde_json::{Map, Value};

use automate_api::ConnectionId;

use super::{
    Connection, Integration, IntegrationContext, IntegrationInfo, SetupComplete, SetupRedirect,
};
use crate::config::Config;
use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::prelude::*;
use crate::services::{GitHubAppClient, GitHubInstallation};

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

pub struct GitHubAppIntegration;

crate::register_integration!(GitHubAppIntegration);

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
pub async fn record_installation(
    installation: &GitHubInstallation,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<(), human_errors::Error> {
    link_installation(installation, services).await?;

    Ok(())
}

/// Creates, or refreshes, the connection standing for an installation.
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
        store.update_secret(existing.id, secret).await?;
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

/// The connection standing for a particular installation, if this account holds one.
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

    /// Turns the installation id the browser came back with into a fact.
    ///
    /// The query string is under the browser's control, so an id in it is a
    /// claim; only the App's own credentials can confirm that the installation
    /// exists and say which account it is on.
    async fn resolve_installation(
        ctx: &IntegrationContext<'_>,
        installation_id: u64,
    ) -> Result<GitHubInstallation, human_errors::Error> {
        Self::client(ctx)?
            .installations()
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
    use automate_api::ConnectionKind;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::services::{AppContext, ServicesContainer};

    type TestServices = ServicesContainer<crate::db::TenantDb>;

    async fn services() -> TestServices {
        TestServices::new_mock().await.expect("build mock services")
    }

    /// A GitHub which reports these installations, and a context whose App is
    /// pointed at it.
    ///
    /// A configured App is the precondition for the setup wizard existing at
    /// all, and the listing is what a completion turns the browser's claimed
    /// installation id into a fact against — so standing one up is what lets
    /// these tests run the callback whole, exactly as the running agent does,
    /// rather than a build of it with the GitHub call cut out.
    ///
    /// The server is handed back with the context because dropping it stops it
    /// listening: a test that let it go would leave the App addressing a closed
    /// port.
    async fn github_reporting(installations: &[GitHubInstallation]) -> (MockServer, AppContext) {
        let server = MockServer::start().await;

        let reported: Vec<Value> = installations
            .iter()
            .map(|installation| {
                serde_json::json!({
                    "id": installation.id,
                    "account": {
                        "login": installation.account,
                        "type": installation.account_type,
                    },
                })
            })
            .collect();

        Mock::given(method("GET"))
            .and(path("/app/installations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reported))
            .mount(&server)
            .await;

        let api_url = server.uri();
        let context = AppContext::new_mock(move |config| {
            config.connections.github.app = Some(crate::testing::github_app(api_url));
        })
        .await
        .expect("build a mock context");

        (server, context)
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

    /// The whole point of collapsing the two records: the integrations page and
    /// the connections page cannot disagree, because there is only one thing for
    /// them to read. A rename made on one shows up on the other.
    #[tokio::test]
    async fn the_integrations_page_and_the_connections_page_describe_the_same_record() {
        let (_github, context) = github_reporting(&[]).await;
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
        let (_github, context) = github_reporting(&[]).await;

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
        let (_github, context) = github_reporting(&[]).await;

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
        let (_github, context) = github_reporting(&[installation(42, "SierraSoftworks")]).await;

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
        let (_github, context) = github_reporting(&[installation(42, "SierraSoftworks")]).await;

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
        let (_github, context) = github_reporting(&[installation(42, "SierraSoftworks")]).await;

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
        let (_github, context) = github_reporting(&[installation(42, "SierraSoftworks")]).await;

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
        let (_github, context) = github_reporting(&[installation(42, "SierraSoftworks")]).await;

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
        let (_github, context) = github_reporting(&[]).await;

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
        let (_github, context) = github_reporting(&[installation(42, "SierraSoftworks")]).await;

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
