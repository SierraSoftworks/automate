use std::borrow::Cow;

use crate::{db::KeyValueStore, services::Services};

pub trait DifferentialCollector {
    type Item;
    type Identifier: Eq + std::hash::Hash + serde::Serialize + serde::de::DeserializeOwned + Send + 'static;

    fn kind(&self) -> &'static str;

    fn partition(&self, namespace: Option<&'static str>) -> String {
        if let Some(ns) = namespace {
            format!("collector::{ns}::{}", self.kind())
        } else {
            format!("collector::{}", self.kind())
        }
    }

    fn key(&self) -> Cow<'static, str>;

    fn identifier(&self, item: &Self::Item) -> Self::Identifier;

    async fn fetch(&self) -> Result<Vec<Self::Item>, human_errors::Error>;

    async fn list(&self, services: &impl Services) -> Result<Vec<Self::Item>, human_errors::Error> {
        let partition = self.partition(None);
        let key = self.key();

        let new_items = self.fetch().await?;
        let known_identifiers: Vec<Self::Identifier> = services.kv().get(partition.clone(), key.clone()).await?.unwrap_or_default();
        let known_set: std::collections::HashSet<_> = known_identifiers.into_iter().collect();
        let filtered_items: Vec<_> = new_items
            .into_iter()
            .filter(|item| {
                let id = self.identifier(item);
                !known_set.contains(&id)
            })
            .collect();

        let new_identifiers: Vec<_> = filtered_items
            .iter()
            .map(|item| self.identifier(item))
            .collect();

        services.kv().set(partition, key, new_identifiers).await?;

        Ok(filtered_items)
    }
}