use std::sync::Arc;

use crate::config::ConnectionConfigs;

pub trait Services {
    fn kv(&self) -> Arc<impl crate::db::KeyValueStore + Send + Sync>;
    fn queue(&self) -> Arc<impl crate::db::Queue + Send + Sync>;
    fn cache(&self) -> Arc<impl crate::db::Cache + Send + Sync>;

    fn connections(&self) -> Arc<ConnectionConfigs>;
}

pub struct ServicesContainer<D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache> {
    pub database: Arc<D>,
    pub connections: Arc<ConnectionConfigs>,
}

impl<D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache> Clone for ServicesContainer<D> {
    fn clone(&self) -> Self {
        Self {
            database: self.database.clone(),
            connections: self.connections.clone(),
        }
    }
}

impl <D> ServicesContainer<D>
where
    D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache,
{
    pub fn new(database: D, connections: ConnectionConfigs) -> Self {
        Self {
            database: Arc::new(database),
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
    D: crate::db::KeyValueStore + crate::db::Queue + crate::db::Cache + Send + Sync,
{
    fn kv(&self) -> Arc<impl crate::db::KeyValueStore + Send + Sync> {
        self.database.clone()
    }

    fn queue(&self) -> Arc<impl crate::db::Queue + Send + Sync> {
        self.database.clone()
    }

    fn cache(&self) -> Arc<impl crate::db::Cache + Send + Sync> {
        self.database.clone()
    }

    fn connections(&self) -> Arc<ConnectionConfigs> {
        self.connections.clone()
    }
}