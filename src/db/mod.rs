use human_errors as errors;
mod sqlite;

pub fn open(path: &str) -> Result<impl KeyValueStore + Queue, errors::Error> {
    sqlite::SqliteDatabase::open(path)
}

pub trait KeyValueStore {
    fn get<T: serde::de::DeserializeOwned>(&self, partition: &str, key: &str) -> Result<Option<T>, errors::Error>;

    fn list<T: serde::de::DeserializeOwned>(&self, partition: &str) -> Result<Vec<(String, T)>, errors::Error>;

    fn set<T: serde::Serialize>(&self, partition: &str, key: &str, value: T) -> Result<(), errors::Error>;

    fn remove(&self, partition: &str, key: &str) -> Result<(), errors::Error>;

}

pub trait Queue {
    fn enqueue<T: serde::Serialize>(&self, partition: &str, job: T, delay: Option<chrono::Duration>) -> Result<(), errors::Error>;

    fn dequeue<T: serde::de::DeserializeOwned>(&self, partition: &str, reserve_for: chrono::Duration) -> Result<Vec<QueueMessage<T>>, errors::Error>;

    fn complete<T>(&self, partition: &str, msg: QueueMessage<T>) -> Result<(), errors::Error>;
}

pub struct QueueMessage<T> {
    pub id: usize,
    pub reservation_id: String,
    pub payload: T,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
}