use std::{borrow::Cow, collections::HashMap, sync::Arc};

use human_errors::{self as errors};
use tokio_rusqlite::{Connection, OptionalExtension};

use super::{ADVICE_DB_ERROR, ADVICE_REPORT_DEV, AuditEntry, AuditQuery, AuditRecord, AuditStore};
use crate::prelude::*;

#[derive(Clone)]
pub struct SqliteDatabase {
    connection: Arc<Connection>,
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
    // Migration 6: rename partitions to hierarchical /‑delimited scheme
    "UPDATE kv SET partition = 'calendar/ics'        WHERE partition = 'collector::calendar';
     UPDATE kv SET partition = 'github/notifications' WHERE partition = 'collector::github_notifications';
     UPDATE kv SET partition = 'github/releases'      WHERE partition = 'collector::github_releases';
     UPDATE kv SET partition = 'rss/feed'             WHERE partition = 'collector::rss';
     UPDATE kv SET partition = 'spotify/liked-tracks' WHERE partition = 'collector::spotify/tracks';
     UPDATE queues SET partition = 'calendar/todoist'               WHERE partition = 'workflow/calendar-todoist';
     UPDATE queues SET partition = 'github/notifications/todoist'   WHERE partition = 'workflow/github-notifications-todoist';
     UPDATE queues SET partition = 'github/notifications/cleanup'   WHERE partition = 'workflow/github-notifications-cleanup';
     UPDATE queues SET partition = 'github/releases/todoist'        WHERE partition = 'workflow/github-releases-todoist';
     UPDATE queues SET partition = 'rss/todoist'                    WHERE partition = 'workflow/rss-todoist';
     UPDATE queues SET partition = 'spotify/yearly-playlist'        WHERE partition = 'workflow/spotify-yearly-playlist';
     UPDATE queues SET partition = 'xkcd/todoist'                   WHERE partition = 'workflow/xkcd-todoist';
     UPDATE queues SET partition = 'youtube/todoist'                WHERE partition = 'workflow/youtube-todoist';",
    // Migration 7: wrap legacy collector watermarks (which were stored as bare
    // RFC 3339 date strings) in the JSON object format introduced alongside the
    // optional ETag/Last-Modified validators. The `json_type` guard keeps the
    // migration idempotent and avoids touching values already in the new form.
    "UPDATE kv \
     SET value = json_object('published', json(value)) \
     WHERE partition IN ('rss/feed', 'github/releases') \
     AND json_valid(value) \
     AND json_type(value) = 'text';",
    // Migration 8: record whether a message's key was chosen by the caller or
    // generated for it. The row key has always been the caller's idempotency key
    // when one was supplied and a random UUID otherwise, but the two were
    // indistinguishable once written. Keeping the caller's key here lets a job
    // re-enqueue itself under the identity it was given (see `JobContext::key`)
    // without mistaking a generated UUID for a meaningful one. Rows written
    // before this migration are left NULL, i.e. treated as generated.
    "ALTER TABLE queues ADD COLUMN idempotencyKey TEXT",
    // Migration 9: namespace the key/value store by tenant.
    //
    // The tenant has to join the primary key rather than sit beside it, so that
    // two users can hold the same key in the same partition without colliding.
    // SQLite cannot alter a primary key in place, so the table is rebuilt.
    // Existing rows belong to the local tenant, which is what an installation
    // with no identity provider configured uses, so a single-tenant install
    // carries on unchanged.
    "CREATE TABLE kv_migrated (
        tenant TEXT NOT NULL,
        partition TEXT NOT NULL,
        key TEXT NOT NULL,
        value TEXT NOT NULL,
        PRIMARY KEY (tenant, partition, key)
    );
     INSERT INTO kv_migrated (tenant, partition, key, value)
        SELECT '!local', partition, key, value FROM kv;
     DROP TABLE kv;
     ALTER TABLE kv_migrated RENAME TO kv;",
    // Migration 10: namespace the queues by tenant, as above.
    //
    // Two indexes replace the single partition index. The tenant-leading one
    // serves a scoped consumer, while the second serves the shared worker's
    // cross-tenant dequeue, which orders by scheduling time across everyone.
    "CREATE TABLE queues_migrated (
        tenant TEXT NOT NULL,
        partition TEXT NOT NULL,
        key TEXT NOT NULL,
        payload TEXT,
        scheduledAt DATETIME DEFAULT CURRENT_TIMESTAMP,
        hiddenUntil DATETIME DEFAULT CURRENT_TIMESTAMP,
        reservedBy TEXT,
        traceparent TEXT,
        tracestate TEXT,
        idempotencyKey TEXT,
        PRIMARY KEY (tenant, partition, key)
    );
     INSERT INTO queues_migrated (
        tenant, partition, key, payload, scheduledAt, hiddenUntil, reservedBy,
        traceparent, tracestate, idempotencyKey
     )
        SELECT '!local', partition, key, payload, scheduledAt, hiddenUntil, reservedBy,
               traceparent, tracestate, idempotencyKey FROM queues;
     DROP TABLE queues;
     ALTER TABLE queues_migrated RENAME TO queues;
     CREATE INDEX idx_queues_tenant_partition_hidden ON queues (tenant, partition, hiddenUntil);
     CREATE INDEX idx_queues_hidden_scheduled ON queues (hiddenUntil, scheduledAt);",
    // Migration 11: the audit log.
    //
    // Entries are ordered by id rather than by timestamp, because several
    // commonly share a timestamp and only the id gives a total order that is
    // stable across queries and usable as a pagination cursor. Both indexes
    // descend for the same reason: every query wants the most recent first.
    "CREATE TABLE audit_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        tenant TEXT NOT NULL,
        occurredAt DATETIME NOT NULL,
        category TEXT NOT NULL,
        action TEXT NOT NULL,
        outcome TEXT NOT NULL,
        subject TEXT,
        actor TEXT,
        message TEXT,
        detail TEXT
    );
     CREATE INDEX idx_audit_tenant ON audit_log (tenant, id DESC);
     CREATE INDEX idx_audit_subject ON audit_log (tenant, subject, id DESC);",
];

impl SqliteDatabase {
    pub async fn open(path: &str) -> Result<Self, errors::Error> {
        let connection = Connection::open(path).await.wrap_user_err(
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
        let connection = Connection::open_in_memory().await.or_system_err(&[
            "Make sure that there is enough memory available to create an in-memory database.",
        ])?;

        let mut db = Self {
            connection: Arc::new(connection),
        };
        db.initialize().await?;

        Ok(db)
    }

    /// Opens a database frozen partway through the migration history.
    ///
    /// Lets a test stand up the schema as an older release left it, populate it,
    /// and then run the remaining migrations against realistic data — which is
    /// the only way to catch a migration that works on an empty table but loses
    /// or corrupts rows on a real one.
    #[cfg(test)]
    pub async fn open_in_memory_at_migration(version: usize) -> Result<Self, errors::Error> {
        let connection = Connection::open_in_memory().await.or_system_err(&[
            "Make sure that there is enough memory available to create an in-memory database.",
        ])?;

        connection
            .call(move |c| {
                c.execute(
                    "CREATE TABLE IF NOT EXISTS migrations (id INTEGER PRIMARY KEY)",
                    [],
                )?;

                for (i, migration) in MIGRATIONS.iter().enumerate().take(version) {
                    c.execute_batch(migration)?;
                    c.execute("INSERT INTO migrations (id) VALUES (?1)", [i + 1])?;
                }

                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await
            .wrap_system_err(
                "Failed to build a database at a historical migration.",
                ADVICE_REPORT_DEV,
            )?;

        Ok(Self {
            connection: Arc::new(connection),
        })
    }

    /// Runs any migrations the database has not yet had applied.
    #[cfg(test)]
    pub async fn upgrade(&mut self) -> Result<(), errors::Error> {
        self.initialize().await
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
            .wrap_system_err(
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
            .wrap_system_err(
                "Failed to determine the latest database migration version.",
                ADVICE_DB_ERROR,
            )?;

        for (i, migration) in MIGRATIONS.iter().enumerate().skip(latest_migration) {
            self.connection
                .call(move |c| {
                    let transaction = c.transaction()?;
                    transaction.execute_batch(migration)?;
                    transaction.execute("INSERT INTO migrations (id) VALUES (?1)", [i + 1])?;

                    transaction.commit()
                })
                .await
                .wrap_system_err(
                    format!("Failed to apply database migration v{}.", i + 1),
                    ADVICE_REPORT_DEV,
                )?;
        }

        Ok(())
    }

    /// A view of the database restricted to a single tenant.
    ///
    /// The returned handle is the only way to reach the storage traits, and it
    /// carries no method that names a tenant. Anything holding one — every job
    /// handler, and every request handler acting on a user's behalf — is
    /// therefore structurally unable to read or write another tenant's records,
    /// rather than merely being expected not to.
    pub fn tenant(&self, tenant: TenantId) -> TenantDb {
        TenantDb {
            connection: self.connection.clone(),
            tenant,
        }
    }

    /// Every tenant with at least one stored record.
    ///
    /// Used by startup reconciliation, which has to visit each tenant in turn
    /// because the scoped handles above cannot enumerate their peers.
    #[allow(dead_code)]
    pub async fn tenants(&self) -> Result<Vec<TenantId>, errors::Error> {
        self.connection
            .call(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT tenant FROM kv \
                         UNION SELECT tenant FROM queues \
                         ORDER BY tenant ASC",
                    )
                    .or_system_err(ADVICE_DB_ERROR)?;

                let iter = stmt
                    .query_map([], |row| row.get::<_, String>(0))
                    .or_system_err(ADVICE_DB_ERROR)?;

                iter.collect::<Result<Vec<_>, _>>()
                    .or_system_err(ADVICE_DB_ERROR)
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)
            .map(|names| names.iter().map(TenantId::from_storage).collect())
    }

    /// Reads audit entries across every tenant.
    ///
    /// The tenant-scoped handle can only see its own history; this is the
    /// administrative view, and is deliberately only reachable from the root.
    #[allow(dead_code)]
    pub async fn audit_all(
        &self,
        query: super::AuditQuery,
    ) -> Result<Vec<super::AuditRecord>, errors::Error> {
        super::audit::audit(&self.connection, None, query).await
    }

    /// Trims the audit log back to the configured retention.
    #[allow(dead_code)]
    pub async fn prune_audit_log(
        &self,
        retain_for: chrono::Duration,
        max_per_tenant: usize,
    ) -> Result<usize, errors::Error> {
        super::audit::prune(&self.connection, retain_for, max_per_tenant).await
    }
}

/// A handle to the database scoped to a single tenant.
///
/// Every statement it issues is constrained to [`TenantDb::tenant`], and the
/// type deliberately exposes no way to change or widen that scope. Isolation is
/// therefore a property of the type rather than of the discipline of each call
/// site.
#[derive(Clone)]
pub struct TenantDb {
    connection: Arc<Connection>,
    tenant: TenantId,
}

impl TenantDb {
    /// The tenant this handle is scoped to.
    #[allow(dead_code)]
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }
}

#[async_trait::async_trait]
impl AuditStore for TenantDb {
    #[instrument("db.sqlite.audit_record", skip(self, entry), fields(otel.kind=?OpenTelemetrySpanKind::Client, audit.category = entry.category_of().as_str(), audit.outcome = entry.outcome_of().as_str()), err(Display))]
    async fn record(&self, entry: AuditEntry) -> Result<(), errors::Error> {
        super::audit::record(&self.connection, &self.tenant, entry).await
    }

    #[instrument("db.sqlite.audit_read", skip(self, query), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    async fn audit(&self, query: AuditQuery) -> Result<Vec<AuditRecord>, errors::Error> {
        super::audit::audit(&self.connection, Some(&self.tenant), query).await
    }
}

#[async_trait::async_trait]
impl KeyValueStore for TenantDb {
    #[instrument("db.sqlite.get", skip(self, partition, key), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    async fn get<T: DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> std::result::Result<Option<T>, errors::Error> {
        let key = key.into().into_owned();
        let partition = partition.into().into_owned();
        let tenant = self.tenant.to_string();

        Ok(self
            .connection
            .call(|c| {
                c.query_one(
                    "SELECT value FROM kv WHERE tenant = ?1 AND partition = ?2 AND key = ?3",
                    (tenant, partition, key),
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
            .or_system_err(ADVICE_REPORT_DEV)?)
    }

    #[instrument("db.sqlite.list", skip(self, partition), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    async fn list<T: DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
    ) -> std::result::Result<Vec<(String, T)>, errors::Error> {
        let partition = partition.into();
        let tenant = self.tenant.to_string();
        self.connection
            .call(move |c| {
                let mut stmt = c
                    .prepare("SELECT key, value FROM kv WHERE tenant = ?1 AND partition = ?2")
                    .or_system_err(ADVICE_DB_ERROR)?;

                let query_iter = stmt
                    .query_map((&tenant, &*partition), |r| {
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
                    .or_system_err(ADVICE_DB_ERROR)?;

                query_iter
                    .collect::<Result<Vec<_>, _>>()
                    .or_system_err(ADVICE_DB_ERROR)
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)
    }

    #[instrument("db.sqlite.set", skip(self, partition, key, value), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    async fn set<T: serde::Serialize + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
        value: T,
    ) -> std::result::Result<(), errors::Error> {
        let serialized = serde_json::to_string(&value).wrap_system_err(
            "Failed to serialize value for storage in the key/value store.",
            ADVICE_REPORT_DEV,
        )?;

        let partition = partition.into().into_owned();
        let key = key.into().into_owned();
        let tenant = self.tenant.to_string();

        self.connection
            .call(move |c| {
                c.execute(
                    "INSERT INTO kv (tenant, partition, key, value) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(tenant, partition, key) DO UPDATE SET value = excluded.value",
                    (tenant, partition, key, serialized),
                )
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)?;
        Ok(())
    }

    #[instrument("db.sqlite.remove", skip(self, partition, key), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    async fn remove(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> std::result::Result<(), errors::Error> {
        let partition = partition.into().into_owned();
        let key = key.into().into_owned();
        let tenant = self.tenant.to_string();

        self.connection
            .call(move |c| {
                c.execute(
                    "DELETE FROM kv WHERE tenant = ?1 AND partition = ?2 AND key = ?3",
                    (tenant, partition, key),
                )
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)?;
        Ok(())
    }

    #[instrument("db.sqlite.kv_partitions", skip(self), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    async fn partitions(&self) -> std::result::Result<Vec<String>, errors::Error> {
        let tenant = self.tenant.to_string();
        self.connection
            .call(move |c| {
                let mut stmt = c
                    .prepare("SELECT DISTINCT partition FROM kv WHERE tenant = ?1 ORDER BY partition ASC")
                    .or_system_err(ADVICE_DB_ERROR)?;

                let iter = stmt
                    .query_map([&tenant], |row| row.get(0))
                    .or_system_err(ADVICE_DB_ERROR)?;

                iter.collect::<Result<Vec<_>, _>>()
                    .or_system_err(ADVICE_DB_ERROR)
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)
    }

    #[instrument("db.sqlite.kv_scan", skip(self), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    async fn scan<T: DeserializeOwned + Send + 'static>(
        &self,
    ) -> std::result::Result<Vec<(String, String, T)>, errors::Error> {
        let tenant = self.tenant.to_string();
        self.connection
            .call(move |c| {
                let mut stmt = c
                    .prepare(
                        "SELECT partition, key, value FROM kv WHERE tenant = ?1 \
                         ORDER BY partition ASC, key ASC",
                    )
                    .or_system_err(ADVICE_DB_ERROR)?;

                let iter = stmt
                    .query_map([&tenant], |row| {
                        let partition: String = row.get(0)?;
                        let key: String = row.get(1)?;
                        let value_str: String = row.get(2)?;
                        let value: T = serde_json::from_str(&value_str).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;
                        Ok((partition, key, value))
                    })
                    .or_system_err(ADVICE_DB_ERROR)?;

                iter.collect::<Result<Vec<_>, _>>()
                    .or_system_err(ADVICE_DB_ERROR)
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)
    }
}

#[async_trait::async_trait]
impl Queue for TenantDb {
    #[instrument("db.sqlite.enqueue", skip(self, partition, job, idempotency_key, delay), fields(otel.kind=?OpenTelemetrySpanKind::Producer, job.kind=std::any::type_name::<T>()
    ), err(Display))]
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

        let partition = partition.into().into_owned();
        let tenant = self.tenant.to_string();
        let serialized = serde_json::to_string(&job).wrap_system_err(
            "Failed to serialize the queue message for storage.",
            ADVICE_REPORT_DEV,
        )?;
        let hidden_until = delay
            .map(|d| chrono::Utc::now() + d)
            .unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH);

        // The row key is the caller's idempotency key when they supplied one, and
        // a random UUID otherwise. `idempotencyKey` records which of the two it
        // was, so a consumer can tell a meaningful identity from a generated one.
        let key = idempotency_key
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into());

        self.connection
            .call(move |c| {
                c.execute(
                    "INSERT INTO queues (tenant, partition, key, payload, hiddenUntil, traceparent, tracestate, idempotencyKey) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                        ON CONFLICT (tenant, partition, key)
                        DO UPDATE
                        SET payload = ?4, hiddenUntil = ?5, scheduledAt = CURRENT_TIMESTAMP, reservedBy = NULL, traceparent = ?6, tracestate = ?7, idempotencyKey = ?8",
                    (tenant, partition, &key, &serialized, &hidden_until, trace_headers.get("traceparent"), trace_headers.get("tracestate"), idempotency_key.as_deref()),
                )
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)?;

        Ok(())
    }

    #[instrument("db.sqlite.dequeue", skip(self, partition, reserve_for), fields(otel.kind=?OpenTelemetrySpanKind::Consumer, job.kind=std::any::type_name::<T>()
    ), err(Display))]
    async fn dequeue<P: Into<Cow<'static, str>> + Send, T: DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        reserve_for: chrono::Duration,
    ) -> std::result::Result<super::QueueMessage<T>, errors::Error> {
        let partition = partition.into();
        let tenant = self.tenant.to_string();

        loop {
            let reservation_id = uuid::Uuid::new_v4().to_string();
            let reserved_until = chrono::Utc::now() + reserve_for;

            let partition = partition.clone();
            let tenant = tenant.clone();
            let message = self.connection.call(move |c| {
                let tx = c.transaction().or_system_err(ADVICE_DB_ERROR)?;

                let message = tx.query_one("SELECT key, payload, scheduledAt, traceparent, tracestate, idempotencyKey FROM queues WHERE tenant = ?1 AND partition = ?2 AND hiddenUntil < CURRENT_TIMESTAMP LIMIT 1", (&tenant, &*partition), |row| {
                    let key: String = row.get(0)?;
                    let payload_str: String = row.get(1)?;
                    let scheduled_at: chrono::DateTime<chrono::Utc> = row.get(2)?;
                    let traceparent: Option<String> = row.get(3)?;
                    let tracestate: Option<String> = row.get(4)?;
                    let idempotency_key: Option<String> = row.get(5)?;

                    let payload: T = serde_json::from_str(&payload_str).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;

                    Ok(super::QueueMessage {
                        key,
                        partition: partition.to_string(),
                        reservation_id: reservation_id.clone(),
                        payload,
                        scheduled_at,
                        traceparent,
                        tracestate,
                        idempotency_key,
                    })
                }).optional().or_system_err(ADVICE_DB_ERROR)?;

                if let Some(msg) = &message {
                    tx.execute(
                        "UPDATE queues
                        SET reservedBy = ?1, hiddenUntil = ?2
                        WHERE tenant = ?3 AND partition = ?4 AND key = ?5",
                        (&reservation_id, &reserved_until, &tenant, &partition, &msg.key),
                    ).or_system_err(ADVICE_DB_ERROR)?;
                }

                tx.commit().or_system_err(ADVICE_DB_ERROR)?;

                Result::<_, human_errors::Error>::Ok(message)
            }).await.or_system_err(ADVICE_DB_ERROR)?;

            if let Some(msg) = message {
                return Ok(msg);
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }

    #[instrument("db.sqlite.dequeue_any", skip(self, reserve_for), fields(otel.kind=?OpenTelemetrySpanKind::Consumer), err(Display))]
    async fn dequeue_any(
        &self,
        reserve_for: chrono::Duration,
    ) -> std::result::Result<super::QueueMessage<serde_json::Value>, errors::Error> {
        let tenant = self.tenant.to_string();

        loop {
            let reservation_id = uuid::Uuid::new_v4().to_string();
            let reserved_until = chrono::Utc::now() + reserve_for;
            let tenant = tenant.clone();

            let message = self.connection.call(move |c| {
                let tx = c.transaction().or_system_err(ADVICE_DB_ERROR)?;

                let message = tx.query_one(
                    "SELECT partition, key, payload, scheduledAt, traceparent, tracestate, idempotencyKey FROM queues WHERE tenant = ?1 AND hiddenUntil < CURRENT_TIMESTAMP ORDER BY scheduledAt LIMIT 1",
                    [&tenant],
                    |row| {
                        let partition: String = row.get(0)?;
                        let key: String = row.get(1)?;
                        let payload_str: String = row.get(2)?;
                        let scheduled_at: chrono::DateTime<chrono::Utc> = row.get(3)?;
                        let traceparent: Option<String> = row.get(4)?;
                        let tracestate: Option<String> = row.get(5)?;
                        let idempotency_key: Option<String> = row.get(6)?;

                        let payload: serde_json::Value = serde_json::from_str(&payload_str).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;

                        Ok(super::QueueMessage {
                            key,
                            partition,
                            reservation_id: reservation_id.clone(),
                            payload,
                            scheduled_at,
                            traceparent,
                            tracestate,
                            idempotency_key,
                        })
                    },
                ).optional().or_system_err(ADVICE_DB_ERROR)?;

                if let Some(msg) = &message {
                    tx.execute(
                        "UPDATE queues
                        SET reservedBy = ?1, hiddenUntil = ?2
                        WHERE tenant = ?3 AND partition = ?4 AND key = ?5",
                        (&reservation_id, &reserved_until, &tenant, &msg.partition, &msg.key),
                    ).or_system_err(ADVICE_DB_ERROR)?;
                }

                tx.commit().or_system_err(ADVICE_DB_ERROR)?;

                Result::<_, human_errors::Error>::Ok(message)
            }).await.or_system_err(ADVICE_DB_ERROR)?;

            if let Some(msg) = message {
                return Ok(msg);
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }

    #[instrument("db.sqlite.complete", skip(self, partition, msg), fields(otel.kind=?OpenTelemetrySpanKind::Consumer, job.kind=std::any::type_name::<T>()), err(Display))]
    async fn complete<P: Into<Cow<'static, str>> + Send, T: Send + 'static>(
        &self,
        partition: P,
        msg: super::QueueMessage<T>,
    ) -> std::result::Result<(), errors::Error> {
        let partition = partition.into().into_owned();
        let tenant = self.tenant.to_string();
        self.connection
            .call(move |c| {
                c.execute(
                    "DELETE FROM queues WHERE tenant = ?1 AND partition = ?2 AND key = ?3 AND reservedBy = ?4",
                    (tenant, partition, &msg.key, &msg.reservation_id),
                )
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)?;
        Ok(())
    }

    #[instrument("db.sqlite.reserve", skip(self, partition, key, reservation_id, reserve_for), fields(otel.kind=?OpenTelemetrySpanKind::Consumer), err(Display))]
    async fn reserve<
        P: Into<Cow<'static, str>> + Send,
        K: Into<Cow<'static, str>> + Send,
        R: Into<Cow<'static, str>> + Send,
    >(
        &self,
        partition: P,
        key: K,
        reservation_id: R,
        reserve_for: chrono::Duration,
    ) -> std::result::Result<(), errors::Error> {
        let partition = partition.into().into_owned();
        let key = key.into().into_owned();
        let reservation_id = reservation_id.into().into_owned();
        let tenant = self.tenant.to_string();
        let reserved_until = chrono::Utc::now() + reserve_for;
        self.connection
            .call(move |c| {
                c.execute(
                    "UPDATE queues
                    SET hiddenUntil = ?1
                    WHERE tenant = ?2 AND partition = ?3 AND key = ?4 AND reservedBy = ?5",
                    (&reserved_until, &tenant, &partition, &key, &reservation_id),
                )
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)?;
        Ok(())
    }

    #[instrument("db.sqlite.peek", skip(self, partition, max_items), fields(otel.kind=?OpenTelemetrySpanKind::Client
    ), err(Display))]
    async fn peek<P: Into<Cow<'static, str>> + Send, T: DeserializeOwned + Send + 'static>(
        &self,
        partition: P,
        max_items: usize,
    ) -> std::result::Result<Vec<super::PeekedMessage<T>>, errors::Error> {
        let partition = partition.into();
        let tenant = self.tenant.to_string();
        self.connection
            .call(move |c| {
                let mut stmt = c
                    .prepare(
                        "SELECT key, payload, scheduledAt, hiddenUntil, reservedBy, \
                         traceparent, tracestate, idempotencyKey \
                         FROM queues WHERE tenant = ?1 AND partition = ?2 \
                         ORDER BY scheduledAt ASC LIMIT ?3",
                    )
                    .or_system_err(ADVICE_DB_ERROR)?;

                let iter = stmt
                    .query_map((&tenant, &*partition, max_items as i64), |row| {
                        let key: String = row.get(0)?;
                        let payload_str: String = row.get(1)?;
                        let scheduled_at: chrono::DateTime<chrono::Utc> = row.get(2)?;
                        let hidden_until: chrono::DateTime<chrono::Utc> = row.get(3)?;
                        let reserved_by: Option<String> = row.get(4)?;
                        let traceparent: Option<String> = row.get(5)?;
                        let tracestate: Option<String> = row.get(6)?;
                        let idempotency_key: Option<String> = row.get(7)?;

                        let payload: T = serde_json::from_str(&payload_str).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                1,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;

                        Ok(super::PeekedMessage {
                            key,
                            payload,
                            scheduled_at,
                            hidden_until,
                            reserved_by,
                            traceparent,
                            tracestate,
                            idempotency_key,
                        })
                    })
                    .or_system_err(ADVICE_DB_ERROR)?;

                iter.collect::<Result<Vec<_>, _>>()
                    .or_system_err(ADVICE_DB_ERROR)
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)
    }

    #[instrument("db.sqlite.purge", skip(self, partition, key), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    async fn purge<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send>(
        &self,
        partition: P,
        key: K,
    ) -> std::result::Result<(), errors::Error> {
        let partition = partition.into().into_owned();
        let key = key.into().into_owned();
        let tenant = self.tenant.to_string();
        self.connection
            .call(move |c| {
                c.execute(
                    "DELETE FROM queues WHERE tenant = ?1 AND partition = ?2 AND key = ?3",
                    (tenant, partition, key),
                )
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)?;
        Ok(())
    }

    #[instrument("db.sqlite.queue_partitions", skip(self), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    async fn partitions(&self) -> std::result::Result<Vec<String>, errors::Error> {
        let tenant = self.tenant.to_string();
        self.connection
            .call(move |c| {
                let mut stmt = c
                    .prepare("SELECT DISTINCT partition FROM queues WHERE tenant = ?1 ORDER BY partition ASC")
                    .or_system_err(ADVICE_DB_ERROR)?;

                let iter = stmt
                    .query_map([&tenant], |row| row.get(0))
                    .or_system_err(ADVICE_DB_ERROR)?;

                iter.collect::<Result<Vec<_>, _>>()
                    .or_system_err(ADVICE_DB_ERROR)
            })
            .await
            .or_system_err(ADVICE_DB_ERROR)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::{AuditCategory, AuditOutcome, QueueMessage};

    use super::*;

    /// The migration index at which the tenant column was introduced, i.e. the
    /// schema as the last single-tenant release left it.
    const PRE_TENANT_MIGRATION: usize = 8;

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    fn bob() -> TenantId {
        TenantId::new("bob").unwrap()
    }

    /// The columns and indexes of a table, used to compare schemas.
    async fn schema_of(db: &SqliteDatabase, table: &'static str) -> (Vec<String>, Vec<String>) {
        db.connection
            .call(move |c| {
                let mut columns = c
                    .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                columns.sort();

                let mut indexes = c
                    .prepare(
                        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 AND name NOT LIKE 'sqlite_%'",
                    )?
                    .query_map([table], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                indexes.sort();

                Ok::<_, tokio_rusqlite::Error>((columns, indexes))
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn tenants_cannot_read_each_others_records() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let (alice_db, bob_db) = (db.tenant(alice()), db.tenant(bob()));

        // Deliberately the same partition and key: the whole point is that
        // these are different records.
        alice_db
            .set("notes", "shared", "alice's value")
            .await
            .unwrap();
        bob_db.set("notes", "shared", "bob's value").await.unwrap();

        assert_eq!(
            alice_db.get::<String>("notes", "shared").await.unwrap(),
            Some("alice's value".into())
        );
        assert_eq!(
            bob_db.get::<String>("notes", "shared").await.unwrap(),
            Some("bob's value".into())
        );
    }

    #[tokio::test]
    async fn listing_and_scanning_never_cross_tenants() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let (alice_db, bob_db) = (db.tenant(alice()), db.tenant(bob()));

        alice_db.set("notes", "a", "alice").await.unwrap();
        bob_db.set("notes", "b", "bob").await.unwrap();
        bob_db.set("secrets", "b", "bob").await.unwrap();

        let listed = alice_db.list::<String>("notes").await.unwrap();
        assert_eq!(listed, vec![("a".to_string(), "alice".to_string())]);

        let scanned = alice_db.scan::<String>().await.unwrap();
        assert_eq!(
            scanned.len(),
            1,
            "scan leaked another tenant's records: {scanned:?}"
        );

        // A partition only exists for the tenants that have written to it.
        assert_eq!(
            KeyValueStore::partitions(&alice_db).await.unwrap(),
            vec!["notes"]
        );
        assert_eq!(
            KeyValueStore::partitions(&bob_db).await.unwrap(),
            vec!["notes", "secrets"]
        );
    }

    #[tokio::test]
    async fn removing_a_record_leaves_the_other_tenants_copy_alone() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let (alice_db, bob_db) = (db.tenant(alice()), db.tenant(bob()));

        alice_db.set("notes", "shared", "alice").await.unwrap();
        bob_db.set("notes", "shared", "bob").await.unwrap();

        alice_db.remove("notes", "shared").await.unwrap();

        assert_eq!(
            alice_db.get::<String>("notes", "shared").await.unwrap(),
            None
        );
        assert_eq!(
            bob_db.get::<String>("notes", "shared").await.unwrap(),
            Some("bob".into())
        );
    }

    #[tokio::test]
    async fn queued_messages_are_only_visible_to_their_own_tenant() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let (alice_db, bob_db) = (db.tenant(alice()), db.tenant(bob()));

        // The same idempotency key in the same partition: two distinct messages.
        alice_db
            .enqueue("work", "alice's job", Some("shared".into()), None)
            .await
            .unwrap();
        bob_db
            .enqueue("work", "bob's job", Some("shared".into()), None)
            .await
            .unwrap();

        let peeked = alice_db.peek::<_, String>("work", 10).await.unwrap();
        assert_eq!(peeked.len(), 1);
        assert_eq!(peeked[0].payload, "alice's job");

        let dequeued = alice_db
            .dequeue::<_, String>("work", chrono::Duration::minutes(1))
            .await
            .unwrap();
        assert_eq!(dequeued.payload, "alice's job");

        // Bob's message is untouched by Alice's consumer.
        assert_eq!(bob_db.peek::<_, String>("work", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn completing_and_purging_cannot_reach_another_tenants_message() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let (alice_db, bob_db) = (db.tenant(alice()), db.tenant(bob()));

        alice_db
            .enqueue("work", "alice's job", Some("shared".into()), None)
            .await
            .unwrap();
        bob_db
            .enqueue("work", "bob's job", Some("shared".into()), None)
            .await
            .unwrap();

        // Purging by a key that exists for both tenants must only affect ours.
        alice_db.purge("work", "shared").await.unwrap();

        assert!(
            alice_db
                .peek::<_, String>("work", 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(bob_db.peek::<_, String>("work", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn every_tenant_holding_records_is_enumerable_from_the_root() {
        // Startup reconciliation has to visit each tenant in turn, and the
        // scoped handles deliberately cannot enumerate their peers.
        let db = SqliteDatabase::open_in_memory().await.unwrap();

        db.tenant(alice()).set("notes", "a", "a").await.unwrap();
        db.tenant(bob())
            .enqueue("work", "b", None, None)
            .await
            .unwrap();

        let tenants = db.tenants().await.unwrap();
        assert_eq!(tenants, vec![alice(), bob()]);
    }

    #[tokio::test]
    async fn upgrading_a_single_tenant_database_preserves_its_records() {
        let mut db = SqliteDatabase::open_in_memory_at_migration(PRE_TENANT_MIGRATION)
            .await
            .unwrap();

        // Guard against this test quietly becoming a no-op if the migration
        // list grows and PRE_TENANT_MIGRATION is not moved with it.
        let (columns, _) = schema_of(&db, "kv").await;
        assert!(
            !columns.contains(&"tenant".to_string()),
            "the fixture should start from the schema as it was before tenancy"
        );

        // Data as the last single-tenant release would have left it.
        db.connection
            .call(|c| {
                c.execute_batch(
                    "INSERT INTO kv (partition, key, value) VALUES \
                       ('rss/feed', 'https://example.com/feed', '{\"published\":\"2024-04-15T12:00:00Z\"}'), \
                       ('todoist/task', 'calendar/1', '{\"id\":\"7\",\"hash\":\"abc\"}'); \
                     INSERT INTO queues (partition, key, payload, idempotencyKey) VALUES \
                       ('cron', 'rss/Example', '{\"kind\":\"rss/todoist\"}', 'rss/Example'), \
                       ('todoist/create-task', 'abc123', '{\"title\":\"Hello\"}', NULL);",
                )
            })
            .await
            .unwrap();

        db.upgrade().await.unwrap();

        // Everything must survive, and belong to the tenant a single-tenant
        // install continues to run as.
        let local = db.tenant(TenantId::local());

        let watermark: serde_json::Value = local
            .get("rss/feed", "https://example.com/feed")
            .await
            .unwrap()
            .expect("the collector watermark should survive the upgrade");
        assert_eq!(watermark["published"], "2024-04-15T12:00:00Z");

        let task: serde_json::Value = local
            .get("todoist/task", "calendar/1")
            .await
            .unwrap()
            .expect("the task mapping should survive the upgrade");
        assert_eq!(task["id"], "7");

        let queued = local
            .peek::<_, serde_json::Value>("cron", 10)
            .await
            .unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].key, "rss/Example");
        assert_eq!(
            queued[0].idempotency_key.as_deref(),
            Some("rss/Example"),
            "the caller-supplied key must not be lost in the table rebuild"
        );

        let created = local
            .peek::<_, serde_json::Value>("todoist/create-task", 10)
            .await
            .unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].payload["title"], "Hello");
        assert_eq!(
            created[0].idempotency_key, None,
            "a generated key must stay distinguishable from a caller-supplied one"
        );

        assert_eq!(db.tenants().await.unwrap(), vec![TenantId::local()]);
    }

    #[tokio::test]
    async fn an_upgraded_database_has_the_same_schema_as_a_fresh_one() {
        // Schema drift between fresh installs and upgraded ones produces bugs
        // that only ever reproduce on somebody else's machine.
        let fresh = SqliteDatabase::open_in_memory().await.unwrap();

        let mut upgraded = SqliteDatabase::open_in_memory_at_migration(PRE_TENANT_MIGRATION)
            .await
            .unwrap();
        upgraded.upgrade().await.unwrap();

        for table in ["kv", "queues"] {
            assert_eq!(
                schema_of(&fresh, table).await,
                schema_of(&upgraded, table).await,
                "the '{table}' table differs between a fresh and an upgraded database"
            );
        }
    }

    #[tokio::test]
    async fn the_tenant_column_joins_the_primary_key() {
        // If the tenant sat beside the key rather than within it, two users
        // could not hold the same key in the same partition.
        let db = SqliteDatabase::open_in_memory().await.unwrap();

        for table in ["kv", "queues"] {
            let key_columns: Vec<String> = db
                .connection
                .call(move |c| {
                    c.prepare(&format!(
                        "SELECT name FROM pragma_table_info('{table}') WHERE pk > 0 ORDER BY pk"
                    ))?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()
                })
                .await
                .unwrap();

            assert_eq!(
                key_columns,
                vec!["tenant", "partition", "key"],
                "unexpected primary key on '{table}'"
            );
        }
    }

    #[tokio::test]
    async fn audit_entries_are_returned_most_recent_first() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let alice_db = db.tenant(alice());

        for action in ["first", "second", "third"] {
            alice_db
                .record(AuditEntry::new(
                    AuditCategory::WorkflowRun,
                    action,
                    AuditOutcome::Success,
                ))
                .await
                .unwrap();
        }

        let entries = alice_db.audit(AuditQuery::recent(10)).await.unwrap();
        let actions: Vec<&str> = entries.iter().map(|e| e.action.as_str()).collect();

        // Ordered by id rather than timestamp, which matters precisely here:
        // all three entries land within the same second.
        assert_eq!(actions, vec!["third", "second", "first"]);
    }

    #[tokio::test]
    async fn a_tenant_only_sees_its_own_audit_history() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();

        db.tenant(alice())
            .record(AuditEntry::new(
                AuditCategory::WorkflowRun,
                "ran",
                AuditOutcome::Success,
            ))
            .await
            .unwrap();
        db.tenant(bob())
            .record(AuditEntry::new(
                AuditCategory::Authentication,
                "signed-in",
                AuditOutcome::Success,
            ))
            .await
            .unwrap();

        let alice_entries = db
            .tenant(alice())
            .audit(AuditQuery::recent(10))
            .await
            .unwrap();
        assert_eq!(alice_entries.len(), 1);
        assert_eq!(alice_entries[0].action, "ran");

        // The administrative view spans everyone, and is only reachable from
        // the root handle.
        let everyone = db.audit_all(AuditQuery::recent(10)).await.unwrap();
        assert_eq!(everyone.len(), 2);
    }

    #[tokio::test]
    async fn audit_entries_round_trip_their_full_detail() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let alice_db = db.tenant(alice());

        alice_db
            .record(
                AuditEntry::new(
                    AuditCategory::WebhookDelivery,
                    "signature-rejected",
                    AuditOutcome::Denied,
                )
                .subject("copper-tiger-canyon")
                .actor("admin")
                .message("The delivery signature did not match the configured secret.")
                .detail(serde_json::json!({ "source": "grafana" })),
            )
            .await
            .unwrap();

        let entry = alice_db
            .audit(AuditQuery::recent(1))
            .await
            .unwrap()
            .pop()
            .unwrap();

        assert_eq!(entry.tenant, alice());
        assert_eq!(entry.category, AuditCategory::WebhookDelivery);
        assert_eq!(entry.outcome, AuditOutcome::Denied);
        assert_eq!(entry.subject.as_deref(), Some("copper-tiger-canyon"));
        assert_eq!(entry.actor.as_deref(), Some("admin"));
        assert_eq!(entry.detail.unwrap()["source"], "grafana");
        assert!(entry.occurred_at <= chrono::Utc::now());
    }

    #[tokio::test]
    async fn audit_entries_can_be_narrowed_and_paged() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let alice_db = db.tenant(alice());

        for i in 0..5 {
            alice_db
                .record(
                    AuditEntry::new(AuditCategory::WorkflowRun, "ran", AuditOutcome::Success)
                        .subject(if i % 2 == 0 {
                            "workflow-a"
                        } else {
                            "workflow-b"
                        }),
                )
                .await
                .unwrap();
        }
        alice_db
            .record(AuditEntry::new(
                AuditCategory::Connection,
                "created",
                AuditOutcome::Success,
            ))
            .await
            .unwrap();

        let runs = alice_db
            .audit(AuditQuery::recent(10).in_category(AuditCategory::WorkflowRun))
            .await
            .unwrap();
        assert_eq!(runs.len(), 5);

        let about_a = alice_db
            .audit(AuditQuery::about("workflow-a", 10))
            .await
            .unwrap();
        assert_eq!(about_a.len(), 3);

        // Paging backwards from a returned id yields the next page without
        // repeating or skipping an entry.
        let first_page = alice_db.audit(AuditQuery::recent(2)).await.unwrap();
        let second_page = alice_db
            .audit(AuditQuery::recent(2).before(first_page.last().unwrap().id))
            .await
            .unwrap();

        assert_eq!(second_page.len(), 2);
        assert!(second_page[0].id < first_page[1].id);
    }

    #[tokio::test]
    async fn pruning_applies_both_an_age_and_a_per_tenant_limit() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();

        for tenant in [alice(), bob()] {
            for _ in 0..5 {
                db.tenant(tenant.clone())
                    .record(AuditEntry::new(
                        AuditCategory::WorkflowRun,
                        "ran",
                        AuditOutcome::Success,
                    ))
                    .await
                    .unwrap();
            }
        }

        // The count limit is per tenant, so one noisy user cannot evict
        // everybody else's history.
        db.prune_audit_log(chrono::Duration::days(30), 2)
            .await
            .unwrap();

        for tenant in [alice(), bob()] {
            assert_eq!(
                db.tenant(tenant)
                    .audit(AuditQuery::recent(10))
                    .await
                    .unwrap()
                    .len(),
                2
            );
        }

        // A zero-length retention window expires everything regardless of count.
        db.prune_audit_log(chrono::Duration::zero(), 1000)
            .await
            .unwrap();
        assert!(
            db.audit_all(AuditQuery::recent(10))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn adding_the_audit_log_leaves_existing_records_untouched() {
        // The audit log arrived after tenancy, so an installation upgrading
        // across both must come through with its data intact.
        let mut db = SqliteDatabase::open_in_memory_at_migration(PRE_TENANT_MIGRATION)
            .await
            .unwrap();

        db.connection
            .call(|c| {
                c.execute_batch(
                    "INSERT INTO kv (partition, key, value) VALUES \
                     ('rss/feed', 'https://example.com/feed', '{\"published\":\"2024-04-15T12:00:00Z\"}');",
                )
            })
            .await
            .unwrap();

        db.upgrade().await.unwrap();

        let local = db.tenant(TenantId::local());
        assert!(
            local
                .get::<serde_json::Value>("rss/feed", "https://example.com/feed")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            local
                .audit(AuditQuery::recent(10))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_key_value_store_basic() {
        let db = SqliteDatabase::open_in_memory()
            .await
            .unwrap()
            .tenant(TenantId::local());

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
        let db = SqliteDatabase::open_in_memory()
            .await
            .unwrap()
            .tenant(TenantId::local());

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
        let db = SqliteDatabase::open_in_memory()
            .await
            .unwrap()
            .tenant(TenantId::local());

        let _session = tracing_batteries::Session::new("automate", "0.0.1-test")
            .with_battery(tracing_batteries::Testing);

        let span = tracing::error_span!("test_queue_basic").entered();
        assert!(!span.context().is_telemetry_suppressed());
        assert!(!Span::current().context().is_telemetry_suppressed());
        // The span must carry a valid, sampled trace context for it to be
        // propagated through the queue as a `traceparent`.
        assert!(span.context().span().span_context().is_valid());

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

        let job: QueueMessage<String> = db
            .dequeue("test_queue", chrono::Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(job.payload, "job1");
        assert_ne!(job.traceparent, None);

        db.complete("test_queue", job).await.unwrap();

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

    /// A caller-supplied idempotency key is both the row key *and* recorded as
    /// the message's identity, so a consumer can re-enqueue under it. A generated
    /// key is only the row key, and must not be mistaken for a meaningful one.
    #[tokio::test]
    async fn test_queue_distinguishes_supplied_keys_from_generated_ones() {
        let db = SqliteDatabase::open_in_memory()
            .await
            .unwrap()
            .tenant(TenantId::local());

        db.enqueue("test_queue", "explicit", Some("my-key".into()), None)
            .await
            .unwrap();

        let peeked: Vec<super::super::PeekedMessage<String>> =
            db.peek("test_queue", 10).await.unwrap();
        assert_eq!(peeked.len(), 1);
        assert_eq!(peeked[0].key, "my-key");
        assert_eq!(peeked[0].idempotency_key.as_deref(), Some("my-key"));

        let job: QueueMessage<String> = db
            .dequeue("test_queue", chrono::Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(job.idempotency_key.as_deref(), Some("my-key"));
        db.complete("test_queue", job).await.unwrap();

        db.enqueue("test_queue", "generated", None, None)
            .await
            .unwrap();

        let job: QueueMessage<String> = db
            .dequeue("test_queue", chrono::Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(
            job.idempotency_key, None,
            "a generated key identifies the row but carries no meaning"
        );
        assert!(
            !job.key.is_empty(),
            "the row key is still populated with the generated UUID"
        );
    }

    /// Re-enqueueing under the same key must keep the message's identity rather
    /// than clearing it, since the upsert path rewrites every other column.
    #[tokio::test]
    async fn test_queue_upsert_preserves_the_supplied_key() {
        let db = SqliteDatabase::open_in_memory()
            .await
            .unwrap()
            .tenant(TenantId::local());

        db.enqueue("test_queue", "first", Some("my-key".into()), None)
            .await
            .unwrap();
        db.enqueue("test_queue", "second", Some("my-key".into()), None)
            .await
            .unwrap();

        let peeked: Vec<super::super::PeekedMessage<String>> =
            db.peek("test_queue", 10).await.unwrap();
        assert_eq!(peeked.len(), 1, "the second enqueue replaced the first");
        assert_eq!(peeked[0].payload, "second");
        assert_eq!(peeked[0].idempotency_key.as_deref(), Some("my-key"));
    }

    #[tokio::test]
    async fn test_queue_purge() {
        let db = SqliteDatabase::open_in_memory()
            .await
            .unwrap()
            .tenant(TenantId::local());

        db.enqueue("test_queue", "job1", Some("key1".into()), None)
            .await
            .unwrap();
        db.enqueue("test_queue", "job2", Some("key2".into()), None)
            .await
            .unwrap();

        db.purge("test_queue", "key1").await.unwrap();

        let remaining: Vec<crate::db::PeekedMessage<String>> =
            db.peek("test_queue", 10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key, "key2");

        // Purging a missing key is a no-op rather than an error.
        db.purge("test_queue", "missing").await.unwrap();
    }

    #[tokio::test]
    async fn test_queue_reserve_adjusts_visibility() {
        let db = SqliteDatabase::open_in_memory()
            .await
            .unwrap()
            .tenant(TenantId::local());

        db.enqueue("test_queue", "job1", Some("key1".into()), None)
            .await
            .unwrap();

        // Dequeue with a long reservation so the message is hidden from other
        // consumers for a minute.
        let msg = db.dequeue_any(chrono::Duration::seconds(60)).await.unwrap();
        assert_eq!(msg.key, "key1");

        // While reserved, the message is not available to dequeue again.
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            db.dequeue_any(chrono::Duration::seconds(60)),
        )
        .await;
        assert!(
            blocked.is_err(),
            "message should stay hidden while reserved"
        );

        // Shortening the reservation into the past releases the message so it
        // becomes available again immediately.
        db.reserve(
            msg.partition.clone(),
            msg.key.clone(),
            msg.reservation_id.clone(),
            chrono::Duration::seconds(-1),
        )
        .await
        .unwrap();

        let released = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            db.dequeue_any(chrono::Duration::seconds(60)),
        )
        .await
        .expect("message should be available after its reservation is released")
        .unwrap();
        assert_eq!(released.key, "key1");

        // Reserving with a non-matching reservation id must not release the
        // message held by another consumer.
        db.reserve(
            "test_queue",
            "key1",
            "not-the-holder",
            chrono::Duration::seconds(-1),
        )
        .await
        .unwrap();
        let still_blocked = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            db.dequeue_any(chrono::Duration::seconds(60)),
        )
        .await;
        assert!(
            still_blocked.is_err(),
            "a non-matching reservation id must not release the message"
        );
    }

    #[tokio::test]
    async fn test_migration_wraps_legacy_watermarks() {
        let db = SqliteDatabase::open_in_memory()
            .await
            .unwrap()
            .tenant(TenantId::local());

        // Simulate data written before the watermark format change: bare RFC 3339
        // date strings in the affected partitions, an unaffected partition, and a
        // value already in the new object form (to prove the migration is scoped
        // and idempotent).
        db.connection
            .call(|c| {
                c.execute_batch(
                    "INSERT INTO kv (tenant, partition, key, value) VALUES \
                     ('!local', 'rss/feed', 'https://example.com/feed', '\"2024-04-15T12:00:00Z\"'), \
                     ('!local', 'github/releases', 'https://api.github.com', '\"2024-03-01T10:00:00Z\"'), \
                     ('!local', 'calendar/ics', 'https://example.com/cal', '\"2024-01-01T00:00:00Z\"'), \
                     ('!local', 'rss/feed', 'https://migrated.example.com/feed', '{\"published\":\"2024-02-02T02:02:02Z\"}');",
                )
            })
            .await
            .unwrap();

        // Apply the migration SQL directly. Because `open_in_memory` already ran
        // it against an empty table, re-running it here exercises the conversion
        // against legacy data while also confirming it is idempotent.
        db.connection
            .call(|c| c.execute_batch(MIGRATIONS[6]))
            .await
            .unwrap();

        // Affected partitions are wrapped into the new object form.
        let rss: serde_json::Value = db
            .get("rss/feed", "https://example.com/feed")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rss["published"], "2024-04-15T12:00:00Z");
        assert!(rss.get("etag").is_none());
        assert!(rss.get("last_modified").is_none());

        let releases: serde_json::Value = db
            .get("github/releases", "https://api.github.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(releases["published"], "2024-03-01T10:00:00Z");

        // Unaffected partitions retain their original representation.
        let calendar: serde_json::Value = db
            .get("calendar/ics", "https://example.com/cal")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calendar, serde_json::json!("2024-01-01T00:00:00Z"));

        // Values already in the new form are left untouched.
        let migrated: serde_json::Value = db
            .get("rss/feed", "https://migrated.example.com/feed")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(migrated["published"], "2024-02-02T02:02:02Z");
    }
}
