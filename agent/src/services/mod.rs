use std::sync::Arc;

use automate_api::TenantId;

use crate::config::Config;
use crate::crypto::SecretStore;

mod alphavantage;
pub mod debounce;
mod github;
mod github_app;

pub use alphavantage::AlphaVantageClient;
pub use github::{AutoMergeOutcome, GitHubClient};
pub use github_app::{GitHubAppClient, GitHubInstallation};
use tracing_batteries::Session;

/// The concrete [`Services`] implementation used by the running application and
/// the job consumer.
///
/// Individual job handlers remain generic over the [`Services`] trait so they
/// can be unit-tested with mocks, but the dynamic job dispatch registry needs a
/// single concrete type to remain object-safe.
///
/// The storage handle it carries is scoped to a single tenant, so anything
/// holding an `AppServices` can only reach that tenant's records. Widening the
/// scope requires the root [`crate::db::SqliteDatabase`], which only
/// installation-level code holds.
pub type AppServices = ServicesContainer<crate::db::TenantDb>;

/// The user agent applied to the shared HTTP client used across collectors and
/// publishers.
pub const HTTP_USER_AGENT: &str = "SierraSoftworks/automate";

/// The installation-wide handle from which tenant-scoped [`AppServices`] are
/// derived.
///
/// This is the only thing that can reach across tenants, and it is deliberately
/// held in very few places: `main`, the job consumer, and the parts of the web
/// layer that resolve who a request is acting for. Everything downstream of
/// those receives an [`AppServices`], which cannot widen its own scope.
#[derive(Clone)]
pub struct AppContext {
    config: Arc<Config>,
    database: crate::db::SqliteDatabase,
    secrets: Arc<SecretStore>,
    http_client: reqwest::Client,
    session: Arc<Session>,
}

impl AppContext {
    pub fn new(
        config: crate::config::Config,
        database: crate::db::SqliteDatabase,
        secrets: SecretStore,
        session: Arc<Session>,
    ) -> Self {
        let http_client = reqwest::Client::builder()
            .user_agent(HTTP_USER_AGENT)
            .build()
            .expect("Failed to build the default HTTP client.");

        Self {
            config: Arc::new(config),
            database,
            secrets: Arc::new(secrets),
            http_client,
            session,
        }
    }

    /// Derives the services used to act on one tenant's behalf.
    pub fn tenant(&self, tenant: TenantId) -> AppServices {
        ServicesContainer {
            config: self.config.clone(),
            database: self.database.tenant(tenant),
            secrets: self.secrets.clone(),
            http_client: self.http_client.clone(),
            session: self.session.clone(),
        }
    }

    /// The unscoped database handle, for installation-level work such as
    /// enumerating tenants or reading the cross-tenant audit log.
    pub fn database(&self) -> &crate::db::SqliteDatabase {
        &self.database
    }

    pub fn config(&self) -> Arc<Config> {
        self.config.clone()
    }

    pub fn session(&self) -> &Session {
        &self.session
    }
}

pub trait Services
where
    Self: Sized,
{
    fn config(&self) -> Arc<crate::config::Config>;

    /// The shared telemetry [`Session`].
    ///
    /// Both [`Session::record_event`] and [`Session::record_error`] operate
    /// through a shared reference, so this accessor is all a handler needs to
    /// emit events or record exceptions from anywhere it can reach the
    /// [`Services`].
    #[allow(dead_code)]
    fn session(&self) -> &Session;

    /// Encrypts and decrypts the credentials this installation holds on behalf
    /// of its users.
    ///
    /// Reached through the services rather than a global so that a test can
    /// supply its own key, and so anything touching a secret says so in its
    /// signature.
    #[allow(dead_code)]
    fn secrets(&self) -> &SecretStore;

    fn kv(&self) -> impl crate::db::KeyValueStore + Clone + Send + Sync + 'static;
    fn queue(&self) -> impl crate::db::Queue + Clone + Send + Sync + 'static;
    fn cache(&self) -> impl crate::db::Cache + Clone + Send + Sync + 'static;

    /// The audit log for this tenant.
    fn audit(&self) -> impl crate::db::AuditStore + Clone + Send + Sync + 'static;

    /// A shared [`reqwest::Client`] configured with the default user agent.
    ///
    /// Cloning a [`reqwest::Client`] is cheap and shares the underlying
    /// connection pool, so collectors and publishers should prefer this over
    /// constructing their own clients.
    fn http_client(&self) -> reqwest::Client;
}

pub struct ServicesContainer<
    D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache + crate::db::AuditStore,
> {
    pub config: Arc<Config>,
    pub database: D,
    pub secrets: Arc<SecretStore>,
    pub http_client: reqwest::Client,
    pub session: Arc<Session>,
}

impl<
    D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache + crate::db::AuditStore + Clone,
> Clone for ServicesContainer<D>
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            database: self.database.clone(),
            secrets: self.secrets.clone(),
            http_client: self.http_client.clone(),
            session: self.session.clone(),
        }
    }
}

#[cfg(test)]
impl AppContext {
    /// Builds a root context backed by an in-memory database and a throwaway
    /// encryption key.
    pub async fn new_mock(
        f: impl Sized + FnOnce(&mut Config),
    ) -> Result<Self, human_errors::Error> {
        let database = crate::db::SqliteDatabase::open_in_memory().await?;

        let mut config = Config::default();
        f(&mut config);

        let session = Arc::new(
            Session::new("automate", "0.0.0-test").with_battery(tracing_batteries::Testing),
        );

        Ok(AppContext::new(
            config,
            database,
            SecretStore::ephemeral(),
            session,
        ))
    }
}

#[cfg(test)]
impl ServicesContainer<crate::db::TenantDb> {
    pub async fn new_mock() -> Result<Self, human_errors::Error> {
        Self::new_custom_mock(|_, _| {}).await
    }

    /// Builds mock services, letting the caller adjust the configuration and
    /// seed the database first.
    ///
    /// Routed through [`AppContext`] so that tests construct their services the
    /// same way the running agent does.
    pub async fn new_custom_mock(
        f: impl Sized + FnOnce(&mut Config, &crate::db::TenantDb),
    ) -> Result<Self, human_errors::Error> {
        let root = crate::db::SqliteDatabase::open_in_memory().await?;
        let database = root.tenant(TenantId::local());

        let mut config = Config::default();
        f(&mut config, &database);

        let session = Arc::new(
            Session::new("automate", "0.0.0-test").with_battery(tracing_batteries::Testing),
        );

        Ok(
            AppContext::new(config, root, SecretStore::ephemeral(), session)
                .tenant(TenantId::local()),
        )
    }
}

impl<D> Services for ServicesContainer<D>
where
    D: crate::db::KeyValueStore
        + crate::db::Queue
        + crate::db::Cache
        + crate::db::AuditStore
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn config(&self) -> Arc<crate::config::Config> {
        self.config.clone()
    }

    fn session(&self) -> &Session {
        &self.session
    }

    fn secrets(&self) -> &SecretStore {
        &self.secrets
    }

    fn kv(&self) -> impl crate::db::KeyValueStore + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn queue(&self) -> impl crate::db::Queue + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn cache(&self) -> impl crate::db::Cache + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn audit(&self) -> impl crate::db::AuditStore + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn http_client(&self) -> reqwest::Client {
        self.http_client.clone()
    }
}
