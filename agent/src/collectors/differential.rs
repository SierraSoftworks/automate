use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};
use tracing_batteries::prelude::*;

use crate::prelude::*;

pub enum Diff<ID, V> {
    Added(ID, V),
    Modified(ID, V),
    Removed(ID),
}

#[allow(dead_code)]
pub trait DifferentialCollector: Collector
where
    Self::Item: Serialize + DeserializeOwned + Clone + PartialEq + Send + 'static,
{
    type Identifier: Eq + std::hash::Hash + Serialize + DeserializeOwned + Clone + Send + 'static;

    fn partition(&self) -> &'static str;

    fn key(&self) -> Cow<'static, str>;

    fn identifier(&self, item: &Self::Item) -> Self::Identifier;

    async fn fetch(&self, services: &impl Services)
    -> Result<Vec<Self::Item>, human_errors::Error>;

    #[allow(clippy::type_complexity)]
    #[instrument("collectors.diff", skip(self, services))]
    async fn diff(
        &self,
        services: &impl Services,
    ) -> Result<Vec<Diff<Self::Identifier, Self::Item>>, human_errors::Error> {
        let partition = self.partition();
        let key = self.key();

        let items = self.fetch(services).await?;

        let old_items: Vec<Self::Item> = services
            .kv()
            .get(partition, key.clone())
            .await?
            .unwrap_or_default();

        let old_by_identifier: HashMap<Self::Identifier, Self::Item> = old_items
            .into_iter()
            .map(|item| (self.identifier(&item), item))
            .collect();

        let mut new_identifiers = HashSet::new();
        let mut output = Vec::new();

        for item in items.iter() {
            let id = self.identifier(item);
            new_identifiers.insert(id.clone());

            // Emit the item when it is newly seen or when its serialized content
            // has changed, so downstream filters get a chance to re-evaluate it.
            // Idempotency of the consuming job prevents duplicate side effects.
            match old_by_identifier.get(&id) {
                Some(previous) if previous == item => {}
                Some(_) => output.push(Diff::Modified(id, item.clone())),
                None => output.push(Diff::Added(id, item.clone())),
            }
        }

        for id in old_by_identifier.into_keys() {
            if !new_identifiers.contains(&id) {
                output.push(Diff::Removed(id));
            }
        }

        services.kv().set(partition, key, items).await?;

        Ok(output)
    }
}
