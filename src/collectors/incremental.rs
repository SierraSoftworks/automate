use std::borrow::Cow;
use tracing_batteries::prelude::*;

use crate::{collectors::Collector, db::KeyValueStore, services::Services};

pub trait IncrementalCollector: Collector {
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

    async fn fetch_since(
        &self,
        watermark: Option<Self::Watermark>,
        services: &impl Services,
    ) -> Result<(Vec<Self::Item>, Self::Watermark), human_errors::Error>;

    #[instrument("collectors.fetch", skip(self, services), err(Display))]
    async fn fetch(
        &self,
        services: &impl Services,
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        let partition = self.partition(None);
        let key = self.key();

        let current_watermark = services.kv().get(partition.clone(), key.clone()).await?;

        let (new_items, new_watermark) = self.fetch_since(current_watermark, services).await?;
        services.kv().set(partition, key, new_watermark).await?;

        Ok(new_items)
    }
}
