use std::sync::Arc;

use crate::config::ConnectionConfigs;

pub trait Services
where
    Self: Sized,
{
    fn connections(&self) -> Arc<ConnectionConfigs>;

    fn kv(&self) -> impl crate::db::KeyValueStore + Clone + Send + Sync + 'static;
    fn queue(&self) -> impl crate::db::Queue + Clone + Send + Sync + 'static;
    fn cache(&self) -> impl crate::db::Cache + Clone + Send + Sync + 'static;
}

pub struct ServicesContainer<D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache> {
    pub database: D,
    pub connections: Arc<ConnectionConfigs>,
}

impl<D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache + Clone> Clone
    for ServicesContainer<D>
{
    fn clone(&self) -> Self {
        Self {
            database: self.database.clone(),
            connections: self.connections.clone(),
        }
    }
}

impl<D> ServicesContainer<D>
where
    D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache,
{
    pub fn new(database: D, connections: ConnectionConfigs) -> Self {
        Self {
            database,
            connections: Arc::new(connections),
        }
    }
}

#[cfg(test)]
impl ServicesContainer<crate::db::SqliteDatabase> {
    pub async fn new_mock() -> Result<Self, human_errors::Error> {
        let database = crate::db::SqliteDatabase::open_in_memory().await?;
        let connections = ConnectionConfigs::default();
        Ok(Self::new(database, connections))
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
    fn kv(&self) -> impl crate::db::KeyValueStore + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn queue(&self) -> impl crate::db::Queue + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn cache(&self) -> impl crate::db::Cache + Clone + Send + Sync + 'static {
        self.database.clone()
    }

    fn connections(&self) -> Arc<ConnectionConfigs> {
        self.connections.clone()
    }
}
