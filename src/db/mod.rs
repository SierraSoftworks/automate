use std::{borrow::Cow, pin::Pin};

use human_errors as errors;

mod cache;
mod partition;
mod sqlite;

pub use partition::Partition;
pub use sqlite::SqliteDatabase;

#[allow(dead_code)]
#[async_trait::async_trait]
pub trait KeyValueStore {
    async fn get<
        P: Into<Cow<'static, str>> + Send,
        K: Into<Cow<'static, str>> + Send,
        T: serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        partition: P,
        key: K,
    ) -> Result<Option<T>, errors::Error>;

    async fn list<
        P: Into<Cow<'static, str>> + Send,
        T: serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        partition: P,
    ) -> Result<Vec<(String, T)>, errors::Error>;

    async fn set<
        P: Into<Cow<'static, str>> + Send,
        K: Into<Cow<'static, str>> + Send,
        T: serde::Serialize + Send + 'static,
    >(
        &self,
        partition: P,
        key: K,
        value: T,
    ) -> Result<(), errors::Error>;

    async fn remove<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send>(
        &self,
        partition: P,
        key: K,
    ) -> Result<(), errors::Error>;

    fn partition<T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static>(
        &self,
        name: impl ToString,
    ) -> Partition<Self, T>
    where
        Self: Sized + Clone,
    {
        Partition::new(self.clone(), name.to_string())
    }
}

#[allow(dead_code)]
#[async_trait::async_trait]
pub trait Queue {
    async fn enqueue<P: Into<Cow<'static, str>> + Send, T: serde::Serialize + Send + 'static>(
        &self,
        partition: P,
        job: T,
        delay: Option<chrono::Duration>,
    ) -> Result<(), errors::Error>;

    async fn dequeue<
        P: Into<Cow<'static, str>> + Send,
        T: serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        partition: P,
        reserve_for: chrono::Duration,
    ) -> Result<Option<QueueMessage<T>>, errors::Error>;

    async fn complete<P: Into<Cow<'static, str>> + Send, T: Send + 'static>(
        &self,
        partition: P,
        msg: QueueMessage<T>,
    ) -> Result<(), errors::Error>;

    fn partition<T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static>(
        &self,
        name: impl ToString,
    ) -> Partition<Self, T>
    where
        Self: Sized + Clone,
    {
        Partition::new(self.clone(), name.to_string())
    }
}

#[allow(dead_code)]
#[async_trait::async_trait]
pub trait Cache {
    async fn cached<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send, T, B>(
        &self,
        partition: P,
        key: K,
        builder: B,
        ttl: chrono::Duration,
    ) -> Result<T, human_errors::Error>
    where
        T: serde::de::DeserializeOwned + serde::Serialize + Clone + Send + 'static,
        B: FnOnce() -> Pin<Box<dyn Future<Output = Result<T, human_errors::Error>> + Sync + Send>>
            + Sync
            + Send;

    fn partition<T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static>(
        &self,
        name: impl ToString,
    ) -> Partition<Self, T>
    where
        Self: Sized + Clone,
    {
        Partition::new(self.clone(), name.to_string())
    }
}

pub struct QueueMessage<T> {
    pub id: usize,
    pub reservation_id: String,
    pub payload: T,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
}
