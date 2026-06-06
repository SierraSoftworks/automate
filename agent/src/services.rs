use std::sync::Arc;

use crate::config::Config;

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
}

impl<D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache + Clone> Clone
    for ServicesContainer<D>
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            database: self.database.clone(),
            http_client: self.http_client.clone(),
        }
    }
}

impl<D> ServicesContainer<D>
where
    D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache,
{
    pub fn new(config: crate::config::Config, database: D) -> Self {
        let http_client = reqwest::Client::builder()
            .user_agent(HTTP_USER_AGENT)
            .build()
            .expect("Failed to build the default HTTP client.");

        Self {
            config: Arc::new(config),
            database,
            http_client,
        }
    }
}

#[cfg(test)]
impl ServicesContainer<crate::db::SqliteDatabase> {
    pub async fn new_mock() -> Result<Self, human_errors::Error> {
        let database = crate::db::SqliteDatabase::open_in_memory().await?;
        let config = Config::default();
        Ok(Self::new(config, database))
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
