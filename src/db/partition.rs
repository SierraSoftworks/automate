use super::*;

pub struct Partition<D, T> {
    pub db: D,
    pub name: String,
    _marker: std::marker::PhantomData<T>,
}

impl<D, T> Partition<D, T> {
    pub fn new(db: D, name: impl ToString) -> Self {
        Self {
            db,
            name: name.to_string(),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl<D: Clone, T> Clone for Partition<D, T> {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            name: self.name.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

#[allow(dead_code)]
impl<D: KeyValueStore, T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static>
    Partition<D, T>
{
    pub async fn get(&self, key: String) -> Result<Option<T>, human_errors::Error> {
        self.db.get(self.name.clone(), key).await
    }

    pub async fn set(&self, key: String, value: T) -> Result<(), human_errors::Error> {
        self.db.set(self.name.clone(), key, value).await
    }

    pub async fn remove(&self, key: String) -> Result<(), human_errors::Error> {
        self.db.remove(self.name.clone(), key).await
    }

    pub async fn list(&self) -> Result<Vec<(String, T)>, human_errors::Error> {
        self.db.list(self.name.clone()).await
    }
}

#[allow(dead_code)]
impl<D: Queue, T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static> Partition<D, T> {
    pub async fn enqueue(
        &self,
        item: T,
        delay: Option<chrono::Duration>,
    ) -> Result<(), human_errors::Error> {
        self.db.enqueue(self.name.clone(), item, delay).await
    }

    pub async fn dequeue(
        &self,
        reserve_for: chrono::Duration,
    ) -> Result<Option<QueueMessage<T>>, human_errors::Error> {
        self.db.dequeue(self.name.clone(), reserve_for).await
    }

    pub async fn complete(&self, msg: QueueMessage<T>) -> Result<(), human_errors::Error> {
        self.db.complete(self.name.clone(), msg).await
    }
}

#[allow(dead_code)]
impl<D: Cache, T: serde::Serialize + serde::de::DeserializeOwned + Clone + Send + 'static>
    Partition<D, T>
{
    pub async fn cached<B>(
        &self,
        key: String,
        builder: B,
        ttl: chrono::Duration,
    ) -> Result<T, human_errors::Error>
    where
        B: FnOnce() -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<T, human_errors::Error>> + Sync + Send>,
            > + Sync
            + Send,
    {
        self.db.cached(self.name.clone(), key, builder, ttl).await
    }
}
