use std::borrow::Cow;

use crate::{db::KeyValueStore, services::Services};

pub trait IncrementalCollector {
    type Item;
    type Watermark: Ord + serde::Serialize + serde::de::DeserializeOwned + Send + 'static;

    fn kind(&self) -> &'static str;

    fn partition(&self, namespace: Option<&'static str>) -> String {
        if let Some(ns) = namespace {
            format!("collector::{ns}::{}", self.kind())
        } else {
            format!("collector::{}", self.kind())
        }
    }

    fn key(&self) -> Cow<'static, str>;

    fn watermark(&self, item: &Self::Item) -> Self::Watermark;

    async fn fetch_since(&self, watermark: Option<Self::Watermark>) -> Result<Vec<Self::Item>, human_errors::Error>;
    
    async fn fetch(&self, services: &impl Services) -> Result<Vec<Self::Item>, human_errors::Error> {
        let partition = self.partition(None);
        let key = self.key();

        let current_watermark = services.kv().get(partition.clone(), key.clone()).await?;

        let new_items = self.fetch_since(current_watermark).await?;
        if let Some(new_watermark) = new_items.iter().map(|item| self.watermark(item)).max() {
            services.kv().set(partition, key, new_watermark).await?;
        }

        Ok(new_items)
    }
}