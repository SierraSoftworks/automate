use std::borrow::Cow;

use crate::prelude::*;
use fjall::{self, PartitionCreateOptions};
use tokio::task::spawn_blocking;

pub struct Database {
    kv: fjall::Keyspace,
}

impl Database {
    pub fn open() -> Result<Self, human_errors::Error> {
        Ok(Self {
            kv: fjall::Config::new(".fjall_data/kv").open().wrap_err_as_user(
                "Failed to open the database file due to an internal error.",
                &["Make sure that you have permission to access the database file and that you are not running on a read-only filesystem."]
            )?,
        })
    }
}

#[async_trait::async_trait]
impl KeyValueStore for Database {
    async fn get<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Option<T>, human_errors::Error> {
        let partition = partition.into();
        let key = key.into();
        let kv = self.kv.clone();

        spawn_blocking(move || {
            let partition = kv.open_partition(partition.as_ref(), PartitionCreateOptions::default()).wrap_err_as_system(
                "Failed to open database partition due to an internal error.",
                &[
                    "Make sure that you have permission to access the database partition and that you are not running on a read-only filesystem."
                ])?;

            let item = partition.get(key.as_ref()).wrap_err_as_system(
                "Failed to get the database item due to an internal error.",
                &[
                    "Make sure that you have permission to access the database item and that you are not running on a read-only filesystem."
                ])?;

            Ok(item.map(|item| serde_json::from_slice(&item).unwrap()))
        }).await.wrap_err_as_system("Failed to dispatch asynchronous task to the underlying database.", &[
            "Please report this issue to the development team on GitHub."
        ])?
    }

    async fn list<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Vec<(String, T)>, human_errors::Error> {
        let partition = partition.into();
        let kv = self.kv.clone();
        
        spawn_blocking(move || {
            let partition = kv.open_partition(partition.as_ref(), PartitionCreateOptions::default()).wrap_err_as_system(
                "Failed to open database partition due to an internal error.",
                &[
                    "Make sure that you have permission to access the database partition and that you are not running on a read-only filesystem."
                ])?;

            Ok(partition.prefix("").into_iter().flat_map(|row| {
                row.map(|(key, value)| {
                    serde_json::from_slice(value.as_ref())
                        .map(|v| (String::from_utf8_lossy(key.as_ref()).to_string(), v))
                        .wrap_err_as_system(
                            "Failed to convert the database item into the requested type.",
                            &["Make sure that the database item is of the expected type."]
                        )
                    }).wrap_err_as_system(
                        "Failed to collect rows from the database.",
                        &["Make sure that the database is accessible and not corrupted."])
            }).collect::<Result<Vec<(String, T)>, _>>()?)
        }).await.wrap_err_as_system("Failed to dispatch asynchronous task to the underlying database.", &[
            "Please report this issue to the development team on GitHub."
        ])?
    }

    async fn set<T: serde::Serialize + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
        value: T,
    ) -> Result<(), human_errors::Error> {
        let partition = partition.into();
        let key = key.into();
        let kv = self.kv.clone();
        
        spawn_blocking(move || {
            let partition = kv.open_partition(partition.as_ref(), PartitionCreateOptions::default()).wrap_err_as_system(
                "Failed to open database partition due to an internal error.",
                &[
                    "Make sure that you have permission to access the database partition and that you are not running on a read-only filesystem."
                ])?;

            partition.insert(
                key.as_ref(),
                serde_json::to_vec(&value).wrap_err_as_system(
                    "Failed to serialize the value to be stored in the database.",
                    &["Make sure that the value is serializable."]
                )?,
            ).wrap_err_as_system(
                "Failed to set the database item due to an internal error.",
                &[
                    "Make sure that you have permission to access the database item and that you are not running on a read-only filesystem."
                ])?;

            Ok(())
        }).await.wrap_err_as_system("Failed to dispatch asynchronous task to the underlying database.", &[
            "Please report this issue to the development team on GitHub."
        ])?
    }

    async fn remove(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> Result<(), human_errors::Error> {
        let partition = partition.into();
        let key = key.into();
        let kv = self.kv.clone();
        
        spawn_blocking(move || {
            let partition = kv.open_partition(partition.as_ref(), PartitionCreateOptions::default()).wrap_err_as_system(
                "Failed to open database partition due to an internal error.",
                &[
                    "Make sure that you have permission to access the database partition and that you are not running on a read-only filesystem."
                ])?;

            partition.remove(key.as_ref()).wrap_err_as_system(
                "Failed to remove the database item due to an internal error.",
                &[
                    "Make sure that you have permission to access the database item and that you are not running on a read-only filesystem."
                ])?;

            Ok(())
        }).await.wrap_err_as_system("Failed to dispatch asynchronous task to the underlying database.", &[
            "Please report this issue to the development team on GitHub."
        ])?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use serde::{de::DeserializeOwned, Serialize};
    use std::fmt::Debug;

    #[rstest]
    #[case("part", "key", "abc".to_string())]
    #[tokio::test]
    async fn test_database<T: Serialize + DeserializeOwned + Clone + PartialEq + Send + Debug + 'static>(
        #[case] partition: impl Into<Cow<'static, str>> + Send,
        #[case] key: impl Into<Cow<'static, str>> + Send,
        #[case] value: T,
    ) {
        let partition = partition.into();
        let key = key.into();
        let db = Database::open().unwrap();
        db.set(partition.clone(), key.clone(), value.clone()).await.unwrap();
        let result: Option<T> = db.get(partition.clone(), key.clone()).await.unwrap();
        assert_eq!(result, Some(value));
        db.remove(partition.clone(), key.clone()).await.unwrap();
    }
}
