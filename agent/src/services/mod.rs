use std::sync::Arc;

use crate::config::Config;

mod alphavantage;

pub use alphavantage::AlphaVantageClient;
use tracing_batteries::Session;

/// The concrete [`Services`] implementation used by the running application and
/// the job consumer.
///
/// Individual job handlers remain generic over the [`Services`] trait so they
/// can be unit-tested with mocks, but the dynamic job dispatch registry needs a
/// single concrete type to remain object-safe.
pub type AppServices = ServicesContainer<crate::db::SqliteDatabase>;

/// The user agent applied to the shared HTTP client used across collectors and
/// publishers.
pub const HTTP_USER_AGENT: &str = "SierraSoftworks/automate";

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

    fn kv(&self) -> impl crate::db::KeyValueStore + Clone + Send + Sync + 'static;
    fn queue(&self) -> impl crate::db::Queue + Clone + Send + Sync + 'static;
    fn cache(&self) -> impl crate::db::Cache + Clone + Send + Sync + 'static;

    /// A shared [`reqwest::Client`] configured with the default user agent.
    ///
    /// Cloning a [`reqwest::Client`] is cheap and shares the underlying
    /// connection pool, so collectors and publishers should prefer this over
    /// constructing their own clients.
    fn http_client(&self) -> reqwest::Client;
}

pub struct ServicesContainer<D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache> {
    pub config: Arc<Config>,
    pub database: D,
    pub http_client: reqwest::Client,
    pub session: Arc<Session>,
}

impl<D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache + Clone> Clone
    for ServicesContainer<D>
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            database: self.database.clone(),
            http_client: self.http_client.clone(),
            session: self.session.clone(),
        }
    }
}

impl<D> ServicesContainer<D>
where
    D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache,
{
    pub fn new(config: crate::config::Config, database: D, session: Arc<Session>) -> Self {
        let http_client = reqwest::Client::builder()
            .user_agent(HTTP_USER_AGENT)
            .build()
            .expect("Failed to build the default HTTP client.");

        Self {
            config: Arc::new(config),
            database,
            http_client,
            session,
        }
    }
}

#[cfg(test)]
impl ServicesContainer<crate::db::SqliteDatabase> {
    pub async fn new_mock() -> Result<Self, human_errors::Error> {
        let database = crate::db::SqliteDatabase::open_in_memory().await?;
        let config = Config::default();
        let session = Arc::new(Session::new("automate", "0.0.0-test").with_battery(tracing_batteries::Testing));
        Ok(Self::new(config, database, session))
    }

    pub async fn new_custom_mock(f: impl Sized + FnOnce(&mut Config, &crate::db::SqliteDatabase)) -> Result<Self, human_errors::Error> {
        let database = crate::db::SqliteDatabase::open_in_memory().await?;
        let mut config = Config::default();
        f(&mut config, &database);
        let session = Arc::new(Session::new("automate", "0.0.0-test").with_battery(tracing_batteries::Testing));
        Ok(Self::new(config, database, session))
    }
}

impl<D> Services for ServicesContainer<D>
where
    D: crate::db::KeyValueStore
        + crate::db::Queue
        + crate::db::Cache
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

    fn kv(&self) -> impl crate::db::KeyValueStore + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn queue(&self) -> impl crate::db::Queue + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn cache(&self) -> impl crate::db::Cache + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn http_client(&self) -> reqwest::Client {
        self.http_client.clone()
    }
}
