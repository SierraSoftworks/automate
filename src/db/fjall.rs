use crate::errors;
use fjall;

pub struct Database {
    keyspace: fjall::Keyspace,
}

impl Database {
    pub fn open() -> Result<Self, errors::Error> {
        Ok(Self {
            keyspace: fjall::Config::default().open().map_err(|e| {
                errors::user_with_internal(
                    "Failed to open the database file due to an internal error.",
                    "Make sure that you have permission to access the database file and that you are not running on a read-only filesystem.",
                    e
                )
            })?
        })
    }

    pub fn get<T: TryFrom<fjall::Slice>>(&self, key: &[&str]) -> Result<T, errors::Error>
    where
        <T as TryFrom<fjall::Slice>>::Error: std::error::Error + Send + Sync + 'static,
    {
        let partition = self.keyspace.open_partition(&self.build_partition_key(key), fjall::PartitionCreateOptions::default()).map_err(|e| errors::user_with_internal(
            "Failed to open database partition due to an internal error.",
            "Make sure that you have permission to access the database partition and that you are not running on a read-only filesystem.",
            e
        ))?;
        let item = partition.get(self.build_row_key(key)).map_err(|e| errors::user_with_internal(
            "Failed to get the database item due to an internal error.",
            "Make sure that you have permission to access the database item and that you are not running on a read-only filesystem.",
            e
        ))?;

        if let Some(item) = item {
            item.try_into().map_err(|e| {
                errors::user_with_internal(
                    "Failed to convert the database item into the requested type.",
                    "Make sure that the database item is of the expected type.",
                    e,
                )
            })
        } else {
            Err(errors::user(
                "The requested item does not exist in the database.",
                "Make sure that the item exists and that you have permission to access it.",
            ))
        }
    }

    pub fn list<T: TryFrom<fjall::Slice>>(&self, prefix: &[&str]) -> Result<Vec<(Vec<String>, T)>, errors::Error> 
    where
        <T as TryFrom<fjall::Slice>>::Error: std::error::Error + Send + Sync + 'static,
    {
        let partition = self.keyspace.open_partition(&self.build_partition_key(prefix), fjall::PartitionCreateOptions::default()).map_err(|e| errors::user_with_internal(
            "Failed to open database partition due to an internal error.",
            "Make sure that you have permission to access the database partition and that you are not running on a read-only filesystem.",
            e
        ))?;

        let items = partition.prefix(&format!("{}/", self.build_partition_key(prefix)));

        items
            .into_iter()
            .flat_map(|item| item.map_err(|e| errors::user_with_internal(
                "Failed to list the database items due to an internal error.",
                "Make sure that you have permission to access the database items and that you are not running on a read-only filesystem.",
                e
            ))
            .map(|(key, value)| (key, value.try_into())))
            .map(|result| match result {
                (key, Ok(value)) => {
                    let mut key_parts = prefix.iter().map(|s| s.to_string()).collect::<Vec<_>>();
                    key_parts.push(unescape_key(key.into()));
                    Ok((key_parts, value))
                }
                (_, e) => Err(e),
            })
            .collect::<Result<Vec<(Vec<String>, T)>, _>>()
            .map_err(|e| {
                errors::user_with_internal(
                    "Failed to convert the database items into the requested type.",
                    "Make sure that the database items are of the expected type.",
                    e,
                )
            })
    }

    pub fn set<T: Into<fjall::Slice>>(&self, key: &[&str], value: T) -> Result<(), errors::Error> {
        let partition = self.keyspace.open_partition(&self.build_partition_key(key), fjall::PartitionCreateOptions::default()).map_err(|e| errors::user_with_internal(
            "Failed to open database partition due to an internal error.",
            "Make sure that you have permission to access the database partition and that you are not running on a read-only filesystem.",
            e
        ))?;

        partition.insert(self.build_row_key(key), value).map_err(|e| errors::user_with_internal(
            "Failed to set the database item due to an internal error.",
            "Make sure that you have permission to access the database item and that you are not running on a read-only filesystem.",
            e
        ))?;

        Ok(())
    }

    pub fn remove(&self, key: &[&str]) -> Result<(), errors::Error>
 {
        let partition = self.keyspace.open_partition(&self.build_partition_key(key), fjall::PartitionCreateOptions::default()).map_err(|e| errors::user_with_internal(
            "Failed to open database partition due to an internal error.",
            "Make sure that you have permission to access the database partition and that you are not running on a read-only filesystem.",
            e
        ))?;

        partition.remove(self.build_row_key(key)).map_err(|e| errors::user_with_internal(
            "Failed to remove the database item due to an internal error.",
            "Make sure that you have permission to access the database item and that you are not running on a read-only filesystem.",
            e
        ))?;

        Ok(())
    }

    fn build_partition_key(&self, key: &[&str]) -> String {
        key[..key.len() - 1].iter().map(escape_key).collect::<Vec<_>>().join("/")
    }

    fn build_row_key(&self, key: &[&str]) -> String {
        key.last().unwrap_or(&"").to_string()
    }
}

fn escape_key<K: AsRef<str>>(key: K) -> String {
    key.as_ref().replace('%', "%25").replace('/', "%2F")
}

fn unescape_key<K: AsRef<str>>(key: K) -> String {
    key.as_ref().replace("%2F", "/").replace("%25", "%")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fjall::Slice;
    use rstest::rstest;
    use std::fmt::Debug;

    #[rstest]
    #[case("abc")]
    #[case("a/b/c")]
    #[case("a/b/c%25")]
    #[case("a/b/c%2F")]
    fn test_escaping(#[case] key: &str) {
        if key.contains('/') || key.contains('%') {
            assert_ne!(escape_key(key), key);
        }

        assert_eq!(unescape_key(escape_key(key)), key);
    }

    #[rstest]
    #[case(&["abc"], "abc".to_string())]
    fn test_database<T: TryFrom<Slice> + Into<Slice> + Clone + PartialEq + Debug>(
        #[case] key: &[&str],
        #[case] value: T,
    ) {
        let db = Database::open().unwrap();
        db.set(key, value.clone()).unwrap();
        let result: T = db.get(key).unwrap();
        assert_eq!(result, value);
        db.remove(key).unwrap();
    }
}
