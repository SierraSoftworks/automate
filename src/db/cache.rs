use std::{borrow::Cow, pin::Pin};

use crate::db::{Cache, KeyValueStore};

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheItem<T> {
    value: T,
    expires_at: chrono::DateTime<chrono::Utc>,
}

#[async_trait::async_trait]
impl<KV: KeyValueStore + Sync + Send + 'static> Cache for KV {
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
            + Send,
    {
        let partition = partition.into();
        let key = key.into();

        if let Some(value @ CacheItem::<T> { .. }) =
            self.get(partition.clone(), key.clone()).await?
        {
            if value.expires_at > chrono::Utc::now() {
                return Ok(value.value);
            }
        }

        let value = builder().await?;
        self.set(
            partition,
            key,
            CacheItem {
                value: value.clone(),
                expires_at: chrono::Utc::now() + ttl,
            },
        )
        .await?;

        Ok(value)
    }
}
