use std::{borrow::Cow, sync::{Arc, RwLock}};

use crate::db::{Cache, KeyValueStore, Queue};

const ADVICE_LOCK_POISONED: &[&str] = &[
    "Restart the application and try again.",
    "Report this error to the development team on GitHub.",
];

#[async_trait::async_trait]
impl<KV: KeyValueStore + Sync + Send + 'static> KeyValueStore for Arc<RwLock<KV>> {
    async fn get<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send, T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        key: K,
    ) -> Result<Option<T>, human_errors::Error> {
        self.read()
            .map_err(|e| human_errors::system(format!("{}", e), ADVICE_LOCK_POISONED))?
            .get(partition, key).await
    }

    async fn set<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send, T: serde::Serialize + Send + 'static>(
        &self,
        partition: P,
        key: K,
        value: T,
    ) -> Result<(), human_errors::Error> {
        self.write()
            .map_err(|e| human_errors::system(format!("{}", e), ADVICE_LOCK_POISONED))?
            .set(partition, key, value).await
    }

    async fn remove<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send>(&self, partition: P, key: K) -> Result<(), human_errors::Error> {
        self.write()
            .map_err(|e| human_errors::system(format!("{}", e), ADVICE_LOCK_POISONED))?
            .remove(partition, key).await
    }

    async fn list<P: Into<Cow<'static, str>> + Send, T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
    ) -> Result<Vec<(String, T)>, human_errors::Error> {
        self.read()
            .map_err(|e| human_errors::system(format!("{}", e), ADVICE_LOCK_POISONED))?
            .list(partition).await
    }
}

#[async_trait::async_trait]
impl <Q: Queue + Sync + Send + 'static> Queue for Arc<RwLock<Q>> {
    async fn enqueue<P: Into<Cow<'static, str>> + Send, T: serde::Serialize + Send + 'static>(
        &self,
        partition: P,
        job: T,
        delay: Option<chrono::Duration>,
    ) -> Result<(), human_errors::Error> {
        self.write()
            .map_err(|e| human_errors::system(format!("{}", e), ADVICE_LOCK_POISONED))?
            .enqueue(partition, job, delay).await
    }

    async fn dequeue<P: Into<Cow<'static, str>> + Send, T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        reserve_for: chrono::Duration,
    ) -> Result<Vec<crate::db::QueueMessage<T>>, human_errors::Error> {
        self.read()
            .map_err(|e| human_errors::system(format!("{}", e), ADVICE_LOCK_POISONED))?
            .dequeue(partition, reserve_for).await
    }

    async fn complete<P: Into<Cow<'static, str>> + Send, T: Send + 'static>(
        &self,
        partition: P,
        msg: crate::db::QueueMessage<T>,
    ) -> Result<(), human_errors::Error> {
        self.write()
            .map_err(|e| human_errors::system(format!("{}", e), ADVICE_LOCK_POISONED))?
            .complete(partition, msg).await
    }
}