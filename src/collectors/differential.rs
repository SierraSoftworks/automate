use crate::{db::KeyValueStore, services::Services};

pub trait DifferentialCollector {
    type Item;
    type Identifier: Eq + std::hash::Hash + serde::Serialize + serde::de::DeserializeOwned;

    fn key(&self) -> (String, String);

    fn identifier(&self, item: &Self::Item) -> Self::Identifier;

    async fn fetch(&self) -> Result<Vec<Self::Item>, human_errors::Error>;

    async fn list(&self, services: &impl Services) -> Result<Vec<Self::Item>, human_errors::Error> {
        let (partition, key) = self.key();

        let new_items = self.fetch().await?;
        let known_identifiers: Vec<Self::Identifier> = services.kv().get(&partition, &key)?.unwrap_or_default();
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

        services.kv().set(&partition, &key, &new_identifiers)?;

        Ok(filtered_items)
    }

    async fn cleanup(&self, services: &impl Services) -> Result<(), human_errors::Error> {
        let (partition, key) = self.key();
        services.kv().remove(&partition, &key)?;
        Ok(())
    }
}