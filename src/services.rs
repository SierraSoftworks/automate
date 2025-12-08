use std::sync::Arc;

use crate::config::Config;

pub trait Services
where
    Self: Sized,
{
    fn config(&self) -> &crate::config::Config;

    fn kv(&self) -> impl crate::db::KeyValueStore + Clone + Send + Sync + 'static;
    fn queue(&self) -> impl crate::db::Queue + Clone + Send + Sync + 'static;
    fn cache(&self) -> impl crate::db::Cache + Clone + Send + Sync + 'static;
}

pub struct ServicesContainer<D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache> {
    pub config: Arc<Config>,
    pub database: D,
}

impl<D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache + Clone> Clone
    for ServicesContainer<D>
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            database: self.database.clone(),
        }
    }
}

impl<D> ServicesContainer<D>
where
    D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache,
{
    pub fn new(config: crate::config::Config, database: D) -> Self {
        Self {
            config: Arc::new(config),
            database,
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
    fn config(&self) -> &crate::config::Config {
        todo!("Implement config retrieval for ServicesContainer")
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
}
