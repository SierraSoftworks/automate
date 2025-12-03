use crate::{db::KeyValueStore, services::Services};

pub trait IncrementalCollector {
    type Item;
    type Watermark: Ord + serde::Serialize + serde::de::DeserializeOwned;

    fn key(&self) -> (String, String);

    fn watermark(&self, item: &Self::Item) -> Self::Watermark;

    async fn fetch_since(&self, watermark: Option<Self::Watermark>) -> Result<Vec<Self::Item>, human_errors::Error>;
    
    async fn list(&self, services: &impl Services) -> Result<Vec<Self::Item>, human_errors::Error> {
        let (partition, key) = self.key();

        let current_watermark = services.kv().get(&partition, &key)?;

        let new_items = self.fetch_since(current_watermark).await?;
        if let Some(new_watermark) = new_items.iter().map(|item| self.watermark(item)).max() {
            services.kv().set(&partition, &key, &new_watermark)?;
        }

        Ok(new_items)
    }

    async fn cleanup(&self, services: &impl Services) -> Result<(), human_errors::Error> {
        let (partition, key) = self.key();
        services.kv().remove(&partition, &key)?;
        Ok(())
    }
}