use std::borrow::Cow;
use tracing_batteries::prelude::*;

use crate::db::StateKey;
use crate::prelude::*;

pub trait IncrementalCollector: Collector {
    type Watermark: Ord + Serialize + DeserializeOwned + Send + 'static;

    fn partition(&self) -> &'static str;

    fn key(&self) -> Cow<'static, str>;

    /// Where the watermark this collector remembers is kept, so that it can be
    /// cleared without a second copy of the derivation going stale.
    fn state(&self) -> StateKey {
        StateKey::new(self.partition(), self.key())
    }

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
        let partition = self.partition();
        let key = self.key();

        let current_watermark = services.kv().get(partition, key.clone()).await?;

        let (new_items, new_watermark) = self.fetch_since(current_watermark, services).await?;
        services.kv().set(partition, key, new_watermark).await?;

        Ok(new_items)
    }
}
