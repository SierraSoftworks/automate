use std::{borrow::Cow, collections::HashSet};
use tracing_batteries::prelude::*;

use crate::prelude::*;

pub enum Diff<ID, V> {
    Added(ID, V),
    Removed(ID),
}

#[allow(dead_code)]
pub trait DifferentialCollector: Collector {
    type Identifier: Eq + std::hash::Hash + Serialize + DeserializeOwned + Clone + Send + 'static;

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

    #[allow(clippy::type_complexity)]
    #[instrument("collectors.diff", skip(self, services))]
    async fn diff(
        &self,
        services: &impl Services,
    ) -> Result<Vec<Diff<Self::Identifier, Self::Item>>, human_errors::Error> {
        let partition = self.partition(None);
        let key = self.key();

        let items = self.fetch().await?;

        let old_identifiers: Vec<Self::Identifier> = services
            .kv()
            .get(partition.clone(), key.clone())
            .await?
            .unwrap_or_default();

        let mut new_identifiers = HashSet::new();
        let mut output = Vec::new();

        for item in items.into_iter() {
            let id = self.identifier(&item);
            new_identifiers.insert(id.clone());

            if !old_identifiers.contains(&id) {
                output.push(Diff::Added(id.clone(), item));
            }
        }

        let removed_identifiers = old_identifiers
            .into_iter()
            .filter(|id| !new_identifiers.contains(id));

        for id in removed_identifiers {
            output.push(Diff::Removed(id));
        }

        let new_identifiers: Vec<_> = new_identifiers.into_iter().collect();

        services.kv().set(partition, key, new_identifiers).await?;

        Ok(output)
    }
}
