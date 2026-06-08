use std::{borrow::Cow, pin::Pin};

use human_errors as errors;

mod cache;
mod partition;
mod sqlite;

use crate::prelude::*;
pub use partition::Partition;
pub use sqlite::SqliteDatabase;

#[allow(dead_code)]
#[async_trait::async_trait]
pub trait KeyValueStore {
    async fn get<T: DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Option<T>, errors::Error>;

    async fn list<T: DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Vec<(String, T)>, errors::Error>;

    async fn set<T: Serialize + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
        value: T,
    ) -> Result<(), errors::Error>;

    async fn remove(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> Result<(), errors::Error>;

    async fn partitions(&self) -> Result<Vec<String>, errors::Error>;

    async fn scan<T: DeserializeOwned + Send + 'static>(
        &self,
    ) -> Result<Vec<(String, String, T)>, errors::Error>;

    fn partition<T: Serialize + DeserializeOwned + Send + 'static>(
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
    async fn enqueue<P: Into<Cow<'static, str>> + Send, T: Serialize + Send + 'static>(
        &self,
        partition: P,
        job: T,
        idempotency_key: Option<Cow<'static, str>>,
        delay: Option<chrono::Duration>,
    ) -> Result<(), errors::Error>;

    async fn dequeue<P: Into<Cow<'static, str>> + Send, T: DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        reserve_for: chrono::Duration,
    ) -> Result<QueueMessage<T>, errors::Error>;

    /// Dequeue the next available message from any partition.
    ///
    /// This is used by the single job consumer to process messages across all
    /// registered job partitions. The returned [`QueueMessage::partition`] field
    /// identifies which job handler should process the message.
    async fn dequeue_any(
        &self,
        reserve_for: chrono::Duration,
    ) -> Result<QueueMessage<serde_json::Value>, errors::Error>;

    async fn complete<P: Into<Cow<'static, str>> + Send, T: Send + 'static>(
        &self,
        partition: P,
        msg: QueueMessage<T>,
    ) -> Result<(), errors::Error>;

    /// Adjust the visibility timeout of a message this consumer currently holds,
    /// identified by its reservation id. The message stays hidden for
    /// `reserve_for` from now; if the holder never completes it, it becomes
    /// available again once that window elapses. This lets the job consumer
    /// narrow the generous dequeue reservation down to each job's own timeout,
    /// which doubles as the backoff before a failed message is retried.
    async fn reserve<
        P: Into<Cow<'static, str>> + Send,
        K: Into<Cow<'static, str>> + Send,
        R: Into<Cow<'static, str>> + Send,
    >(
        &self,
        partition: P,
        key: K,
        reservation_id: R,
        reserve_for: chrono::Duration,
    ) -> Result<(), errors::Error>;

    async fn peek<P: Into<Cow<'static, str>> + Send, T: DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        max_items: usize,
    ) -> Result<Vec<PeekedMessage<T>>, errors::Error>;

    /// Remove a message from the queue by its key, regardless of whether it is
    /// currently reserved. This is intended for administrative interventions
    /// such as cancelling a queued job.
    async fn purge<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send>(
        &self,
        partition: P,
        key: K,
    ) -> Result<(), errors::Error>;

    async fn partitions(&self) -> Result<Vec<String>, errors::Error>;

    fn partition<T: Serialize + DeserializeOwned + Send + 'static>(
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
pub struct PeekedMessage<T> {
    pub key: String,
    pub payload: T,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    pub hidden_until: chrono::DateTime<chrono::Utc>,
    pub reserved_by: Option<String>,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
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
        T: DeserializeOwned + Serialize + Clone + Send + 'static,
        B: FnOnce() -> Pin<Box<dyn Future<Output = Result<T, human_errors::Error>> + Sync + Send>>
            + Sync
            + Send;

    fn partition<T: Serialize + DeserializeOwned + Send + 'static>(
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
    pub key: String,
    pub partition: String,
    pub reservation_id: String,
    pub payload: T,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
}

impl<T> OpenTelemetryPropagationExtractor for QueueMessage<T> {
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            "traceparent" => self.traceparent.as_deref(),
            "tracestate" => self.tracestate.as_deref(),
            _ => None,
        }
    }

    fn keys(&self) -> Vec<&str> {
        match (&self.traceparent, &self.tracestate) {
            (Some(_), Some(_)) => vec!["traceparent", "tracestate"],
            (Some(_), None) => vec!["traceparent"],
            (None, Some(_)) => vec!["tracestate"],
            (None, None) => vec![],
        }
    }
}
