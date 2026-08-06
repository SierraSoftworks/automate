//! The audit log.
//!
//! A single append-only table records everything worth being able to look back
//! at: workflow runs, webhook deliveries, changes to connections, and the
//! security events around signing in and impersonation.
//!
//! # Why one table rather than several
//!
//! These could each have been kept in the key-value store beside the records
//! they describe. They are not, for three reasons.
//!
//! The access pattern is wrong for a key-value store. The questions asked of
//! this data are "what happened recently", "what happened to this workflow",
//! and "what has this administrator been doing" — all of which want ordering,
//! filtering and pagination rather than a lookup by key.
//!
//! It is unbounded where the rest of the store is not. A busy webhook can
//! produce thousands of entries a day, so it needs pruning, and pruning wants a
//! table it can delete from in one statement.
//!
//! Most importantly, it is the one place a user can find out *why* something
//! did not work. A webhook whose signing secret is misconfigured is accepted at
//! the edge and then quietly dropped during verification; without a record of
//! that rejection the user sees only silence.
//!
//! # Ordering
//!
//! Entries are ordered by their auto-incrementing id rather than their
//! timestamp. Several entries commonly share a timestamp, and an id gives a
//! total order that is stable across queries and doubles as a pagination
//! cursor.

// Like the storage traits alongside it, this is a capability surface whose
// callers arrive incrementally as workflows and webhooks are built on top.
#![allow(dead_code)]

use std::borrow::Cow;

use human_errors as errors;
use tokio_rusqlite::Connection;

use crate::prelude::*;

// The shape of an entry is part of the REST contract, because the admin UI reads
// the log back. It is defined once, alongside the rest of that contract.
pub use automate_api::{AuditCategory, AuditOutcome, AuditRecord};

/// An entry about to be written to the log.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    category: AuditCategory,
    action: Cow<'static, str>,
    outcome: AuditOutcome,
    subject: Option<String>,
    actor: Option<String>,
    message: Option<String>,
    detail: Option<serde_json::Value>,
}

impl AuditEntry {
    /// Begins an entry.
    ///
    /// `action` names what was attempted, in the past tense and scoped to the
    /// category — `"dispatched"`, `"signature-rejected"`, `"impersonated"`.
    pub fn new(
        category: AuditCategory,
        action: impl Into<Cow<'static, str>>,
        outcome: AuditOutcome,
    ) -> Self {
        Self {
            category,
            action: action.into(),
            outcome,
            subject: None,
            actor: None,
            message: None,
            detail: None,
        }
    }

    /// The thing acted upon: a workflow or connection identifier, or a username.
    pub fn subject(mut self, subject: impl ToString) -> Self {
        self.subject = Some(subject.to_string());
        self
    }

    /// The person responsible, where there was one.
    ///
    /// Left unset for work the agent does on its own initiative, which is how a
    /// scheduled run is told apart from one somebody triggered by hand. When an
    /// administrator is impersonating, this is the administrator rather than
    /// the user whose records are being changed.
    pub fn actor(mut self, actor: impl ToString) -> Self {
        self.actor = Some(actor.to_string());
        self
    }

    /// A sentence explaining the entry, written to be read by the user whose
    /// workflow it concerns rather than by an operator.
    pub fn message(mut self, message: impl ToString) -> Self {
        self.message = Some(message.to_string());
        self
    }

    /// Structured detail for anything that does not belong in the message.
    pub fn detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }

    /// The area of the system this entry concerns.
    pub fn category_of(&self) -> AuditCategory {
        self.category
    }

    /// How the audited operation turned out.
    pub fn outcome_of(&self) -> AuditOutcome {
        self.outcome
    }
}

/// Which entries to read back.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    /// Restrict to one area of the system.
    pub category: Option<AuditCategory>,

    /// Restrict to entries about one workflow, connection or user.
    pub subject: Option<String>,

    /// Return only entries older than this id, for paging backwards through
    /// history.
    pub before: Option<i64>,

    /// How many entries to return.
    pub limit: usize,
}

impl AuditQuery {
    /// The most recent `limit` entries.
    pub fn recent(limit: usize) -> Self {
        Self {
            limit,
            ..Default::default()
        }
    }

    /// The most recent `limit` entries concerning one subject.
    pub fn about(subject: impl ToString, limit: usize) -> Self {
        Self {
            subject: Some(subject.to_string()),
            limit,
            ..Default::default()
        }
    }

    /// Restricts the query to a single category.
    pub fn in_category(mut self, category: AuditCategory) -> Self {
        self.category = Some(category);
        self
    }

    /// Pages backwards from a previously returned [`AuditRecord::id`].
    pub fn before(mut self, id: i64) -> Self {
        self.before = Some(id);
        self
    }
}

/// Reads and writes the audit log for one tenant.
#[allow(dead_code)]
#[async_trait::async_trait]
pub trait AuditStore {
    /// Appends an entry.
    async fn record(&self, entry: AuditEntry) -> Result<(), errors::Error>;

    /// Reads entries back, most recent first.
    async fn audit(&self, query: AuditQuery) -> Result<Vec<AuditRecord>, errors::Error>;
}

/// Builds the `WHERE` clause and bindings shared by the scoped and global reads.
fn read_query(
    tenant: Option<&TenantId>,
    query: &AuditQuery,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;

    let mut conditions: Vec<&str> = Vec::new();
    let mut bindings: Vec<Value> = Vec::new();

    if let Some(tenant) = tenant {
        conditions.push("tenant = ?");
        bindings.push(Value::Text(tenant.to_string()));
    }

    if let Some(category) = query.category {
        conditions.push("category = ?");
        bindings.push(Value::Text(category.as_str().to_string()));
    }

    if let Some(subject) = &query.subject {
        conditions.push("subject = ?");
        bindings.push(Value::Text(subject.clone()));
    }

    if let Some(before) = query.before {
        conditions.push("id < ?");
        bindings.push(Value::Integer(before));
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    bindings.push(Value::Integer(query.limit as i64));

    (
        format!(
            "SELECT id, tenant, occurredAt, category, action, outcome, subject, actor, message, detail \
             FROM audit_log {where_clause} ORDER BY id DESC LIMIT ?"
        ),
        bindings,
    )
}

/// Maps a result row onto an [`AuditRecord`].
fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditRecord> {
    let category: String = row.get(3)?;
    let outcome: String = row.get(5)?;
    let detail: Option<String> = row.get(9)?;

    Ok(AuditRecord {
        id: row.get(0)?,
        tenant: TenantId::from_storage(row.get::<_, String>(1)?),
        occurred_at: row.get(2)?,
        // An unrecognised value means the row was written by a newer release.
        // Reading the rest of the entry is more useful than failing the query.
        category: AuditCategory::parse(&category).unwrap_or(AuditCategory::Administration),
        action: row.get(4)?,
        outcome: AuditOutcome::parse(&outcome).unwrap_or(AuditOutcome::Failure),
        subject: row.get(6)?,
        actor: row.get(7)?,
        message: row.get(8)?,
        detail: detail.and_then(|d| serde_json::from_str(&d).ok()),
    })
}

/// Writes an entry against a given tenant.
pub(super) async fn record(
    connection: &Connection,
    tenant: &TenantId,
    entry: AuditEntry,
) -> Result<(), errors::Error> {
    let tenant = tenant.to_string();
    // Timestamps are bound from here rather than left to CURRENT_TIMESTAMP,
    // which SQLite resolves only to the second.
    let occurred_at = chrono::Utc::now();
    let detail = entry
        .detail
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .wrap_system_err(
            "Failed to serialise the detail of an audit entry.",
            &["Please report this issue to the development team via GitHub."],
        )?;

    connection
        .call(move |c| {
            c.execute(
                "INSERT INTO audit_log \
                 (tenant, occurredAt, category, action, outcome, subject, actor, message, detail) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                (
                    tenant,
                    occurred_at,
                    entry.category.as_str(),
                    entry.action.as_ref(),
                    entry.outcome.as_str(),
                    entry.subject,
                    entry.actor,
                    entry.message,
                    detail,
                ),
            )
        })
        .await
        .or_system_err(super::ADVICE_DB_ERROR)?;

    Ok(())
}

/// Reads entries, optionally restricted to one tenant.
pub(super) async fn audit(
    connection: &Connection,
    tenant: Option<&TenantId>,
    query: AuditQuery,
) -> Result<Vec<AuditRecord>, errors::Error> {
    let (sql, bindings) = read_query(tenant, &query);

    connection
        .call(move |c| {
            let mut stmt = c.prepare(&sql).or_system_err(super::ADVICE_DB_ERROR)?;

            let iter = stmt
                .query_map(rusqlite::params_from_iter(bindings), read_record)
                .or_system_err(super::ADVICE_DB_ERROR)?;

            iter.collect::<Result<Vec<_>, _>>()
                .or_system_err(super::ADVICE_DB_ERROR)
        })
        .await
        .or_system_err(super::ADVICE_DB_ERROR)
}

/// Trims the log back to the configured retention.
///
/// Applies two independent limits, because either alone leaves a gap: an age
/// limit lets a busy webhook fill the disk within the window, and a count limit
/// lets a quiet installation keep entries indefinitely. The count is applied per
/// tenant so that one noisy user cannot evict everybody else's history.
pub(super) async fn prune(
    connection: &Connection,
    retain_for: chrono::Duration,
    max_per_tenant: usize,
) -> Result<usize, errors::Error> {
    let cutoff = chrono::Utc::now() - retain_for;
    let max_per_tenant = max_per_tenant as i64;

    connection
        .call(move |c| {
            let tx = c.transaction()?;

            let by_age = tx.execute("DELETE FROM audit_log WHERE occurredAt < ?1", [cutoff])?;

            let by_count = tx.execute(
                "DELETE FROM audit_log WHERE id IN (
                     SELECT id FROM (
                         SELECT id, ROW_NUMBER() OVER (PARTITION BY tenant ORDER BY id DESC) AS position
                         FROM audit_log
                     ) WHERE position > ?1
                 )",
                [max_per_tenant],
            )?;

            tx.commit()?;

            Ok::<_, tokio_rusqlite::Error>(by_age + by_count)
        })
        .await
        .or_system_err(super::ADVICE_DB_ERROR)
}
