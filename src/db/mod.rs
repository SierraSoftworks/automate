use std::{borrow::Cow, pin::Pin};

use human_errors as errors;

mod cache;
mod sqlite;
//mod sync;

pub use sqlite::SqliteDatabase;

#[async_trait::async_trait]
pub trait KeyValueStore {
    async fn get<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send, T: serde::de::DeserializeOwned + Send + 'static>(&self, partition: P, key: K) -> Result<Option<T>, errors::Error>;

    async fn list<P: Into<Cow<'static, str>> + Send, T: serde::de::DeserializeOwned + Send + 'static>(&self, partition: P) -> Result<Vec<(String, T)>, errors::Error>;

    async fn set<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send, T: serde::Serialize + Send + 'static>(&self, partition: P, key: K, value: T) -> Result<(), errors::Error>;
    async fn remove<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send>(&self, partition: P, key: K) -> Result<(), errors::Error>;
}

#[async_trait::async_trait]
pub trait Queue {
    async fn enqueue<P: Into<Cow<'static, str>> + Send, T: serde::Serialize + Send + 'static>(&self, partition: P, job: T, delay: Option<chrono::Duration>) -> Result<(), errors::Error>;

    async fn dequeue<P: Into<Cow<'static, str>> + Send, T: serde::de::DeserializeOwned + Send + 'static>(&self, partition: P, reserve_for: chrono::Duration) -> Result<Vec<QueueMessage<T>>, errors::Error>;

    async fn complete<P: Into<Cow<'static, str>> + Send, T: Send + 'static>(&self, partition: P, msg: QueueMessage<T>) -> Result<(), errors::Error>;
}

#[async_trait::async_trait]
pub trait Cache {
    async fn cached<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send, T, B>(&self, partition: P, key: K, builder: B, ttl: chrono::Duration) -> Result<T, human_errors::Error>
    where
        T: serde::de::DeserializeOwned + serde::Serialize + Clone + Send + 'static,
        B: FnOnce() -> Pin<Box<dyn Future<Output = Result<T, human_errors::Error>> + Sync + Send>> + Sync + Send;
}

pub struct QueueMessage<T> {
    pub id: usize,
    pub reservation_id: String,
    pub payload: T,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
}