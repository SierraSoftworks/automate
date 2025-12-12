use std::{borrow::Cow, pin::Pin};

use human_errors as errors;

mod cache;
mod partition;
mod fjall;
mod sqlite;

pub use partition::Partition;
pub use sqlite::SqliteDatabase;
use tracing_batteries::prelude::OpenTelemetryPropagationExtractor;

#[allow(dead_code)]
#[async_trait::async_trait]
pub trait KeyValueStore {
    async fn get<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Option<T>, errors::Error>;

    async fn list<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Vec<(String, T)>, errors::Error>;

    async fn set<T: serde::Serialize + Send + 'static>(
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
        idempotency_key: Option<Cow<'static, str>>,
        delay: Option<chrono::Duration>,
    ) -> Result<(), errors::Error>;

    async fn dequeue<
        P: Into<Cow<'static, str>> + Send,
        T: serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        partition: P,
        reserve_for: chrono::Duration,
    ) -> Result<QueueMessage<T>, errors::Error>;

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
    pub key: String,
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
