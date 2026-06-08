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

        // Load the previously seen items. If the stored state can't be loaded or
        // deserialized (for example because the storage format changed between
        // versions), discard it and start from an empty baseline rather than
        // blocking the collector. The consuming jobs are idempotent, so
        // replaying items is safe.
        let old_items: Vec<Self::Item> = match services.kv().get(partition, key.clone()).await {
            Ok(old_items) => old_items.unwrap_or_default(),
            Err(err) => {
                warn!(
                    collector.partition = partition,
                    collector.key = %key,
                    error = %err,
                    "Failed to load previous collector state; discarding it and continuing from an empty baseline."
                );
                Vec::new()
            }
        };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::Services;

    #[derive(Clone, PartialEq, Serialize, Deserialize)]
    struct TestItem {
        id: u32,
    }

    struct TestCollector {
        items: Vec<TestItem>,
    }

    #[async_trait::async_trait]
    impl Collector for TestCollector {
        type Item = TestItem;

        async fn list(
            &self,
            _services: &(impl Services + Send + Sync + 'static),
        ) -> Result<Vec<Self::Item>, human_errors::Error> {
            Ok(self.items.clone())
        }
    }

    impl DifferentialCollector for TestCollector {
        type Identifier = u32;

        fn partition(&self) -> &'static str {
            "test/differential"
        }

        fn key(&self) -> Cow<'static, str> {
            Cow::Borrowed("key")
        }

        fn identifier(&self, item: &Self::Item) -> Self::Identifier {
            item.id
        }

        async fn fetch(
            &self,
            _services: &impl Services,
        ) -> Result<Vec<Self::Item>, human_errors::Error> {
            Ok(self.items.clone())
        }
    }

    #[tokio::test]
    async fn test_diff_discards_unreadable_state() {
        let services = crate::testing::mock_services().await.unwrap();

        // Seed the store with a value that cannot be deserialized into the
        // collector's `Vec<TestItem>` state, simulating a state-format change
        // between versions (as happened with the calendar collector).
        services
            .kv()
            .set("test/differential", "key", "not a list of items")
            .await
            .unwrap();

        let collector = TestCollector {
            items: vec![TestItem { id: 1 }, TestItem { id: 2 }],
        };

        // The unreadable state is discarded rather than blocking the collector,
        // so every fetched item is treated as newly added.
        let diffs = collector.diff(&services).await.unwrap();
        assert_eq!(diffs.len(), 2);
        assert!(diffs.iter().all(|d| matches!(d, Diff::Added(_, _))));

        // The store now holds the fresh, well-formed state, so a subsequent run
        // observes no changes.
        let diffs = collector.diff(&services).await.unwrap();
        assert!(diffs.is_empty());
    }
}
