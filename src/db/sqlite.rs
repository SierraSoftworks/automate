use std::{borrow::Cow, collections::HashMap, sync::Arc};

use human_errors::{self as errors, ResultExt};
use tokio_rusqlite::{Connection, OptionalExtension};
use tracing_batteries::prelude::*;

use crate::db::{KeyValueStore, Queue};

#[derive(Clone)]
pub struct SqliteDatabase {
    connection: Arc<Connection>,
}

const ADVICE_DB_ERROR: &[&str] = &[
    "Make sure that the database file is accessible and not corrupted.",
    "If the problem persists, please report the issue to the development team via GitHub.",
];

const ADVICE_REPORT_DEV: &[&str] =
    &["Please report this issue to the development team via GitHub."];

impl SqliteDatabase {
    pub async fn open(path: &str) -> Result<Self, errors::Error> {
        let connection = Connection::open(path).await.wrap_err_as_user(
            format!("Unable to open SQLite database file '{path}'."),
            &["Make sure the file path is correct and accessible."],
        )?;

        let mut db = Self {
            connection: Arc::new(connection),
        };
        db.initialize().await?;

        Ok(db)
    }

    #[cfg(test)]
    pub async fn open_in_memory() -> Result<Self, errors::Error> {
        let connection = Connection::open_in_memory().await.map_err_as_system(&[
            "Make sure that there is enough memory available to create an in-memory database.",
        ])?;

        let mut db = Self {
            connection: Arc::new(connection),
        };
        db.initialize().await?;

        Ok(db)
    }

    async fn initialize(&mut self) -> Result<(), errors::Error> {
        self.connection
            .call(|c| {
                c.execute(
                    "CREATE TABLE IF NOT EXISTS migrations (
                    id INTEGER PRIMARY KEY
                )",
                    [],
                )
            })
            .await
            .wrap_err_as_system(
                "Failed to initialize the migrations table.",
                ADVICE_DB_ERROR,
            )?;

        let latest_migration: usize = self
            .connection
            .call(|c| {
                c.query_one("SELECT COALESCE(MAX(id), 0) FROM migrations", [], |r| {
                    r.get(0)
                })
            })
            .await
            .wrap_err_as_system(
                "Failed to determine the latest database migration version.",
                ADVICE_DB_ERROR,
            )?;

        for (i, migration) in MIGRATIONS.iter().enumerate().skip(latest_migration) {
            self.connection
                .call(move |c| {
                    let transaction = c.transaction()?;
                    transaction.execute(migration, [])?;
                    transaction.execute("INSERT INTO migrations (id) VALUES (?1)", [i + 1])?;

                    transaction.commit()
                })
                .await
                .wrap_err_as_system(
                    format!("Failed to apply database migration v{}.", i + 1),
                    ADVICE_REPORT_DEV,
                )?;
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl KeyValueStore for SqliteDatabase {
    #[instrument("db.sqlite.get", skip(self, partition, key), err(Display))]
    async fn get<
        T: serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> std::result::Result<Option<T>, errors::Error> {
        let key = key.into();
        let partition = partition.into();

        Ok(self
            .connection
            .call(|c| {
                c.query_one(
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
                .optional()
            })
            .await
            .map_err_as_system(ADVICE_REPORT_DEV)?)
    }

    #[instrument("db.sqlite.list", skip(self, partition), err(Display))]
    async fn list<
        T: serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
    ) -> std::result::Result<Vec<(String, T)>, errors::Error> {
        let partition = partition.into();
        self.connection
            .call(move |c| {
                let mut stmt = c
                    .prepare("SELECT key, value FROM kv WHERE partition = ?1")
                    .map_err_as_system(ADVICE_DB_ERROR)?;

                let query_iter = stmt
                    .query_map([&partition], |r| {
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
                    })
                    .map_err_as_system(ADVICE_DB_ERROR)?;

                query_iter
                    .collect::<Result<Vec<_>, _>>()
                    .map_err_as_system(ADVICE_DB_ERROR)
            })
            .await
            .map_err_as_system(ADVICE_DB_ERROR)
    }

    #[instrument("db.sqlite.set", skip(self, partition, key, value), err(Display))]
    async fn set<
        T: serde::Serialize + Send + 'static,
    >(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
        value: T,
    ) -> std::result::Result<(), errors::Error> {
        let serialized = serde_json::to_string(&value).wrap_err_as_system(
            "Failed to serialize value for storage in the key/value store.",
            ADVICE_REPORT_DEV,
        )?;

        let partition = partition.into();
        let key = key.into();

        self.connection
            .call(move |c| {
                c.execute(
                    "INSERT INTO kv (partition, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(partition, key) DO UPDATE SET value = excluded.value",
                    (partition, key, serialized),
                )
            })
            .await
            .map_err_as_system(ADVICE_DB_ERROR)?;
        Ok(())
    }

    #[instrument("db.sqlite.remove", skip(self, partition, key), err(Display))]
    async fn remove(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> std::result::Result<(), errors::Error> {
        let partition = partition.into();
        let key = key.into();

        self.connection
            .call(move |c| {
                c.execute(
                    "DELETE FROM kv WHERE partition = ?1 AND key = ?2",
                    (partition, key),
                )
            })
            .await
            .map_err_as_system(ADVICE_DB_ERROR)?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Queue for SqliteDatabase {
    #[instrument("db.sqlite.enqueue", skip(self, partition, job, idempotency_key, delay), err(Display))]
    async fn enqueue<P: Into<Cow<'static, str>> + Send, T: serde::Serialize + Send + 'static>(
        &self,
        partition: P,
        job: T,
        idempotency_key: Option<Cow<'static, str>>,
        delay: Option<chrono::Duration>,
    ) -> std::result::Result<(), errors::Error> {
        let mut trace_headers = HashMap::new();
        get_text_map_propagator(|p| {
            p.inject_context(&Span::current().context(), &mut trace_headers);
        });

        let partition = partition.into();
        let serialized = serde_json::to_string(&job).wrap_err_as_system(
            "Failed to serialize the queue message for storage.",
            ADVICE_REPORT_DEV,
        )?;
        let hidden_until = delay
            .map(|d| chrono::Utc::now() + d)
            .unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH);

        let key = idempotency_key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into());

        self.connection
            .call(move |c| {
                c.execute(
                    "INSERT INTO queues (partition, key, payload, hiddenUntil, traceparent, tracestate) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                        ON CONFLICT (partition, key)
                        DO UPDATE
                        SET payload = ?3, hiddenUntil = ?4, scheduledAt = CURRENT_TIMESTAMP, reservedBy = NULL",
                    (partition, &key, &serialized, &hidden_until, trace_headers.get("traceparent"), trace_headers.get("tracestate")),
                )
            })
            .await
            .map_err_as_system(ADVICE_DB_ERROR)?;

        Ok(())
    }

    #[instrument("db.sqlite.dequeue", skip(self, partition, reserve_for), err(Display))]
    async fn dequeue<
        P: Into<Cow<'static, str>> + Send,
        T: serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        partition: P,
        reserve_for: chrono::Duration,
    ) -> std::result::Result<Option<super::QueueMessage<T>>, errors::Error> {
        let reservation_id = uuid::Uuid::new_v4().to_string();
        let reserved_until = chrono::Utc::now() + reserve_for;

        let partition = partition.into();

        self.connection.call(move |c| {
            let tx = c.transaction().map_err_as_system(ADVICE_DB_ERROR)?;

            let message = tx.query_one("SELECT key, payload, scheduledAt, traceparent, tracestate FROM queues WHERE partition = ?1 AND hiddenUntil < CURRENT_TIMESTAMP LIMIT 1", [&partition], |row| {
                let key: String = row.get(0)?;
                let payload_str: String = row.get(1)?;
                let scheduled_at: chrono::DateTime<chrono::Utc> = row.get(2)?;
                let traceparent: Option<String> = row.get(3)?;
                let tracestate: Option<String> = row.get(4)?;

                let payload: T = serde_json::from_str(&payload_str).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;

                Ok(super::QueueMessage {
                    key,
                    reservation_id: reservation_id.clone(),
                    payload,
                    scheduled_at,
                    traceparent,
                    tracestate,
                })
            }).optional().map_err_as_system(ADVICE_DB_ERROR)?;

            if let Some(msg) = &message {
                tx.execute(
                    "UPDATE queues
                    SET reservedBy = ?1, hiddenUntil = ?2
                    WHERE partition = ?3 AND key = ?4",
                    (&reservation_id, &reserved_until, &partition, &msg.key),
                ).map_err_as_system(ADVICE_DB_ERROR)?;
            }

            tx.commit().map_err_as_system(ADVICE_DB_ERROR)?;

            Result::<_, human_errors::Error>::Ok(message)
        }).await.map_err_as_system(ADVICE_DB_ERROR)
    }

    #[instrument("db.sqlite.complete", skip(self, partition, msg), err(Display))]
    async fn complete<P: Into<Cow<'static, str>> + Send, T: Send + 'static>(
        &self,
        partition: P,
        msg: super::QueueMessage<T>,
    ) -> std::result::Result<(), errors::Error> {
        let partition = partition.into();
        self.connection
            .call(move |c| {
                c.execute(
                    "DELETE FROM queues WHERE partition = ?1 AND key = ?2 AND reservedBy = ?3",
                    (partition, &msg.key, &msg.reservation_id),
                )
            })
            .await
            .map_err_as_system(ADVICE_DB_ERROR)?;
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
        key TEXT NOT NULL,
        payload TEXT,
        scheduledAt DATETIME DEFAULT CURRENT_TIMESTAMP,
        hiddenUntil DATETIME DEFAULT CURRENT_TIMESTAMP,
        reservedBy TEXT,
        PRIMARY KEY (partition, key)
    )",
    "CREATE INDEX IF NOT EXISTS idx_queues_partition_hidden ON queues (partition, hiddenUntil)",
    "ALTER TABLE queues ADD COLUMN traceparent TEXT",
    "ALTER TABLE queues ADD COLUMN tracestate TEXT",
];

#[cfg(test)]
mod tests {
    use crate::db::QueueMessage;

    use super::*;

    #[tokio::test]
    async fn test_key_value_store_basic() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();

        assert_eq!(
            Option::<String>::None,
            db.get("test_partition", "non_existent_key").await.unwrap()
        );

        db.set("test_partition", "test_key", "test_value")
            .await
            .unwrap();
        let value: String = db.get("test_partition", "test_key").await.unwrap().unwrap();
        assert_eq!(value, "test_value");

        let list: Vec<(String, String)> = db.list("test_partition").await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], ("test_key".to_string(), "test_value".to_string()));

        db.remove("test_partition", "test_key").await.unwrap();
        let result: Option<String> = db.get("test_partition", "test_key").await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_key_value_store_json() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();

        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug, Clone)]
        struct TestStruct {
            field1: String,
            field2: i32,
        }

        let test_value = TestStruct {
            field1: "value1".to_string(),
            field2: 42,
        };

        db.set("test_partition", "test_key", test_value.clone())
            .await
            .unwrap();
        let value: Option<TestStruct> = db.get("test_partition", "test_key").await.unwrap();
        assert_eq!(value, Some(test_value));
    }

    #[tokio::test]
    async fn test_queue_basic() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();

        db.enqueue("test_queue", "job1", None, None).await.unwrap();

        db.connection
            .call(|c| {
                c.query_one(
                    "SELECT COUNT(*) FROM queues WHERE partition = ?1",
                    ["test_queue"],
                    |r| {
                        let count: i64 = r.get(0)?;
                        assert_eq!(count, 1);
                        Ok(())
                    },
                )
            })
            .await
            .unwrap();

        let job: Option<QueueMessage<String>> = db
            .dequeue("test_queue", chrono::Duration::seconds(60))
            .await
            .unwrap();
        assert!(job.is_some(), "Expected to dequeue a job from the queue");
        assert_eq!(job.as_ref().unwrap().payload, "job1");

        if let Some(job) = job {
            db.complete("test_queue", job).await.unwrap();
        }

        db.connection
            .call(|c| {
                c.query_one("SELECT COUNT(*) FROM queues", [], |r| {
                    let count: i64 = r.get(0)?;
                    assert_eq!(count, 0);
                    Ok(())
                })
            })
            .await
            .unwrap();
    }
}
