use human_errors::{self as errors, ResultExt};
use rusqlite::{Connection, OptionalExtension, Result};

use crate::db::{KeyValueStore, Queue};

pub struct SqliteDatabase {
    connection: Connection,
}

const ADVICE_DB_ERROR: &[&str] = &[
    "Make sure that the database file is accessible and not corrupted.",
    "If the problem persists, please report the issue to the development team via GitHub.",
];

const ADVICE_REPORT_DEV: &[&str] = &[
    "Please report this issue to the development team via GitHub.",
];

impl SqliteDatabase {
    pub fn open(path: &str) -> Result<Self, errors::Error> {
        let connection = Connection::open(path).wrap_err_as_user(
            format!("Unable to open SQLite database file '{path}'."),
            &["Make sure the file path is correct and accessible."],
        )?;

        let mut db = Self { connection };
        db.initialize()?;

        Ok(db)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, errors::Error> {
        let connection = Connection::open_in_memory().map_err_as_system(&[
            "Make sure that there is enough memory available to create an in-memory database.",
        ])?;

        let mut db = Self { connection };
        db.initialize()?;

        Ok(db)
    }

    fn initialize(&mut self) -> Result<(), errors::Error> {
        self.connection
            .execute(
                "CREATE TABLE IF NOT EXISTS migrations (
                id INTEGER PRIMARY KEY
            )",
                [],
            )
            .wrap_err_as_system(
                "Failed to initialize the migrations table.",
                ADVICE_DB_ERROR,
            )?;

        let latest_migration: usize =
            self.connection
                .query_one("SELECT COALESCE(MAX(id), 0) FROM migrations", [], |r| {
                    r.get(0)
                }).wrap_err_as_system(
                    "Failed to determine the latest database migration version.", 
                    ADVICE_DB_ERROR,
                )?;

        for (i, migration) in MIGRATIONS.iter().enumerate().skip(latest_migration) {
            let transaction = self.connection.transaction().map_err_as_system(ADVICE_DB_ERROR)?;
            transaction.execute(migration, []).wrap_err_as_system(
                format!("Failed to apply database migration v{}.", i+1),
                ADVICE_REPORT_DEV,
            )?;
            transaction.execute("INSERT INTO migrations (id) VALUES (?1)", [i + 1]).map_err_as_system(ADVICE_DB_ERROR)?;

            transaction.commit().map_err_as_system(ADVICE_DB_ERROR)?;
        }

        Ok(())
    }
}

impl KeyValueStore for SqliteDatabase {
    fn get<T: serde::de::DeserializeOwned>(
        &self,
        partition: &str,
        key: &str,
    ) -> std::result::Result<Option<T>, errors::Error> {
        Ok(self
            .connection
            .query_one(
                "SELECT value FROM kv WHERE partition = ?1 AND key = ?2",
                [partition, key],
                |r| {
                    let value: String = r.get(0)?;
                    let deserialized: T = serde_json::from_str(&value).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                    Ok(deserialized)
                },
            )
            .optional().map_err_as_system(ADVICE_REPORT_DEV)?)
    }

    fn list<T: serde::de::DeserializeOwned>(
        &self,
        partition: &str,
    ) -> std::result::Result<Vec<(String, T)>, errors::Error> {
        let mut stmt = self
            .connection
            .prepare("SELECT key, value FROM kv WHERE partition = ?1")
            .map_err_as_system(ADVICE_REPORT_DEV)?;
        let rows = stmt.query_map([partition], |r| {
            let key: String = r.get(0)?;
            let value: String = r.get(1)?;
            let deserialized: T = serde_json::from_str(&value).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok((key, deserialized))
        }).map_err_as_system(ADVICE_DB_ERROR)?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err_as_system(ADVICE_DB_ERROR)?);
        }

        Ok(results)
    }

    fn set<T: serde::Serialize>(
        &self,
        partition: &str,
        key: &str,
        value: T,
    ) -> std::result::Result<(), errors::Error> {
        let serialized = serde_json::to_string(&value).wrap_err_as_system(
            "Failed to serialize value for storage in the key/value store.", 
            ADVICE_REPORT_DEV,
        )?;

        self.connection.execute(
            "INSERT INTO kv (partition, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(partition, key) DO UPDATE SET value = excluded.value",
            [partition, key, &serialized],
        ).map_err_as_system(ADVICE_DB_ERROR)?;

        Ok(())
    }

    fn remove(&self, partition: &str, key: &str) -> std::result::Result<(), errors::Error> {
        self.connection.execute(
            "DELETE FROM kv WHERE partition = ?1 AND key = ?2",
            [partition, key],
        ).map_err_as_system(ADVICE_DB_ERROR)?;
        Ok(())
    }
}

impl Queue for SqliteDatabase {
    fn enqueue<T: serde::Serialize>(
        &self,
        partition: &str,
        job: T,
        delay: Option<chrono::Duration>,
    ) -> std::result::Result<(), errors::Error> {
        let serialized = serde_json::to_string(&job).wrap_err_as_system(
            "Failed to serialize the queue message for storage.",
            ADVICE_REPORT_DEV,
        )?;
        let hidden_until = delay
            .map(|d| chrono::Utc::now() + d)
            .unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH);

        self.connection.execute(
            "INSERT INTO queues (partition, payload, hiddenUntil) VALUES (?1, ?2, ?3)",
            (partition, &serialized, &hidden_until),
        ).map_err_as_system(ADVICE_DB_ERROR)?;

        Ok(())
    }

    fn dequeue<T: serde::de::DeserializeOwned>(
        &self,
        partition: &str,
        reserve_for: chrono::Duration,
    ) -> std::result::Result<Vec<super::QueueMessage<T>>, errors::Error> {
        let reservation_id = uuid::Uuid::new_v4().to_string();
        let reserved_until = chrono::Utc::now() + reserve_for;

        self.connection.execute(
            "UPDATE queues
             SET reservedBy = ?1, hiddenUntil = ?2
             WHERE partition = ?3 AND hiddenUntil < CURRENT_TIMESTAMP",
            (&reservation_id, &reserved_until, partition),
        ).map_err_as_system(ADVICE_DB_ERROR)?;

        let mut stmt = self.connection.prepare(
            "SELECT id, payload, scheduledAt FROM queues
             WHERE partition = ?1 AND reservedBy = ?2 AND hiddenUntil <= ?3",
        ).map_err_as_system(ADVICE_DB_ERROR)?;
        let queue_iter = stmt.query_map((partition, &reservation_id, &reserved_until), |row| {
            let id: usize = row.get(0)?;
            let payload_str: String = row.get(1)?;
            let scheduled_at: chrono::DateTime<chrono::Utc> = row.get(2)?;

            let payload: T = serde_json::from_str(&payload_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

            Ok(super::QueueMessage {
                id,
                reservation_id: reservation_id.clone(),
                payload,
                scheduled_at,
            })
        }).map_err_as_system(ADVICE_DB_ERROR)?;

        Ok(queue_iter.collect::<Result<Vec<super::QueueMessage<T>>, _>>().map_err_as_system(ADVICE_DB_ERROR)?)
    }

    fn complete<T>(
        &self,
        partition: &str,
        msg: super::QueueMessage<T>,
    ) -> std::result::Result<(), errors::Error> {
        self.connection.execute(
            "DELETE FROM queues WHERE partition = ?1 AND id = ?2 AND reservedBy = ?3",
            (partition, &msg.id, &msg.reservation_id),
        ).map_err_as_system(ADVICE_DB_ERROR)?;
        Ok(())
    }
}

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS kv (
        partition TEXT NOT NULL,
        key TEXT NOT NULL,
        value TEXT NOT NULL,
        PRIMARY KEY (partition, key)
    )",
    "CREATE TABLE IF NOT EXISTS queues (
        partition TEXT NOT NULL,
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        payload TEXT NOT NULL,
        scheduledAt DATETIME DEFAULT CURRENT_TIMESTAMP,
        hiddenUntil DATETIME DEFAULT CURRENT_TIMESTAMP,
        reservedBy TEXT
    )",
    "CREATE INDEX IF NOT EXISTS idx_queues_partition_hidden ON queues (partition, hiddenUntil)",
];

#[cfg(test)]
mod tests {
    use crate::db::QueueMessage;

    use super::*;

    #[test]
    fn test_key_value_store_basic() {
        let db = SqliteDatabase::open_in_memory().unwrap();

        assert_eq!(
            None,
            db.get::<String>("test_partition", "non_existent_key")
                .unwrap()
        );

        db.set("test_partition", "test_key", "test_value").unwrap();
        let value: String = db.get("test_partition", "test_key").unwrap().unwrap();
        assert_eq!(value, "test_value");

        let list: Vec<(String, String)> = db.list("test_partition").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], ("test_key".to_string(), "test_value".to_string()));

        db.remove("test_partition", "test_key").unwrap();
        let result: Option<String> = db.get("test_partition", "test_key").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_key_value_store_json() {
        let db = SqliteDatabase::open_in_memory().unwrap();

        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct TestStruct {
            field1: String,
            field2: i32,
        }

        let test_value = TestStruct {
            field1: "value1".to_string(),
            field2: 42,
        };

        db.set("test_partition", "test_key", &test_value).unwrap();
        let value: Option<TestStruct> = db.get("test_partition", "test_key").unwrap();
        assert_eq!(value, Some(test_value));
    }

    #[test]
    fn test_queue_basic() {
        let db = SqliteDatabase::open_in_memory().unwrap();

        db.enqueue("test_queue", "job1", None).unwrap();

        db.connection
            .query_one("SELECT COUNT(*) FROM queues", [], |r| {
                let count: i64 = r.get(0)?;
                assert_eq!(count, 1);
                Ok(())
            })
            .unwrap();

        let jobs: Vec<QueueMessage<String>> = db
            .dequeue("test_queue", chrono::Duration::seconds(60))
            .unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].payload, "job1");

        for job in jobs.into_iter() {
            db.complete("test_queue", job).unwrap();
        }

        db.connection
            .query_one("SELECT COUNT(*) FROM queues", [], |r| {
                let count: i64 = r.get(0)?;
                assert_eq!(count, 0);
                Ok(())
            })
            .unwrap();
    }
}
