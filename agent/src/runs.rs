//! Where a workflow's runs are remembered.
//!
//! # Why this is not the audit log
//!
//! Every run used to write an audit entry. That is fine for a feed polled every
//! six hours and hopeless for a GitHub App installed on a busy organisation:
//! thousands of deliveries a day, each one a row, and the entries somebody
//! actually wanted — a connection replaced, a workflow paused, a run that
//! started failing — pushed off the end of the first page within the hour.
//!
//! The log is now told about a run only when the answer changes. Everything
//! else lives here, as one record per workflow that is overwritten in place, so
//! a workflow which runs a thousand times a day costs exactly what one that
//! runs twice does.
//!
//! # Why the input is kept
//!
//! "It failed" is not a thing anybody can act on. The payload it failed on
//! usually is — which delivery, which fields, which of the twelve repositories.
//! So the record carries what the run was handed, redacted and capped, and the
//! most recent failure is kept alongside the most recent run: on a busy
//! workflow the run that failed at three in the morning has been overwritten
//! several hundred times by breakfast, which is precisely when somebody comes
//! looking for it.

use std::collections::HashMap;

use automate_api::{RunOutcome, RunReport, RunState, WorkflowHealth, WorkflowId};
use human_errors::Error;
use serde_json::Value;

use crate::db::KeyValueStore;
use crate::prelude::*;

/// The partition holding one run record per workflow.
///
/// Named for what it holds rather than scoped under a workflow type, because it
/// is read across all of them when a list is drawn.
pub const RUNS_PARTITION: &str = "runs";

/// The most of a run's input we will keep.
///
/// A GitHub `push` on a large repository runs to hundreds of kilobytes, and the
/// record is meant to answer "what did this look like" rather than to be a copy
/// of the delivery. Anything larger is kept as a prefix, which still identifies
/// the delivery even when it cannot reproduce it.
const MAX_INPUT_BYTES: usize = 16 * 1024;

/// How much of an oversized input is kept as a prefix.
const PREVIEW_BYTES: usize = 2 * 1024;

const REDACTED: &str = "<redacted>";

/// Fragments of a field name that mean its value must not be written down.
///
/// Matched as substrings so that `x-hub-signature-256`, `authorization` and
/// `client_secret` are all caught without enumerating them. The record is
/// stored in the key-value store, which the admin UI can browse and an
/// administrator can read across accounts — so a webhook signature landing here
/// is a credential leak, not untidiness.
const SENSITIVE: &[&str] = &[
    "authorization",
    "cookie",
    "credential",
    "password",
    "secret",
    "signature",
    "token",
];

fn is_sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SENSITIVE.iter().any(|needle| key.contains(needle))
}

/// Replaces the values of sensitive-looking fields, at any depth.
fn redact(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive(key) {
                        Value::String(REDACTED.to_string())
                    } else {
                        redact(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// What of a run's input is safe and small enough to keep.
pub fn keepable(input: &Value) -> Option<Value> {
    if input.is_null() {
        return None;
    }

    let redacted = redact(input);

    let Ok(encoded) = serde_json::to_string(&redacted) else {
        return None;
    };

    if encoded.len() <= MAX_INPUT_BYTES {
        return Some(redacted);
    }

    // Cut on a character boundary, since the prefix is going back out as JSON.
    let mut end = PREVIEW_BYTES.min(encoded.len());
    while !encoded.is_char_boundary(end) {
        end -= 1;
    }

    Some(serde_json::json!({
        "truncated": true,
        "bytes": encoded.len(),
        "preview": &encoded[..end],
    }))
}

/// What a run changed about a workflow's health.
///
/// The audit log is written from this rather than from the run, which is what
/// bounds it: an incident is two entries however many runs it spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// A working workflow still works, or a failing one still fails.
    Unchanged,

    /// The first failure after a run that worked.
    StartedFailing,

    /// The first success after `after` failures.
    Recovered { after: u32 },
}

/// Reads and writes the run records belonging to one tenant.
pub struct RunStore<S> {
    services: S,
}

impl<S: Services> RunStore<S> {
    pub fn new(services: S) -> Self {
        Self { services }
    }

    /// Everything kept about one workflow's runs, payloads included.
    pub async fn get(&self, workflow: WorkflowId) -> Result<Option<RunState>, Error> {
        self.services
            .kv()
            .get(RUNS_PARTITION, workflow.to_string())
            .await
    }

    /// How every workflow is getting on, for drawing a list.
    ///
    /// One read for the whole account rather than one per workflow, and without
    /// the payloads, which are the large part.
    pub async fn health(&self) -> Result<HashMap<WorkflowId, WorkflowHealth>, Error> {
        let stored: Vec<(String, RunState)> = self.services.kv().list(RUNS_PARTITION).await?;

        Ok(stored
            .into_iter()
            .filter_map(|(key, state)| {
                // A key that no longer parses belonged to a workflow from an
                // older identifier scheme; there is nothing to attach it to.
                key.parse().ok().map(|id| (id, state.health()))
            })
            .collect())
    }

    /// Records how a run went, and says whether that changed anything.
    pub async fn record(
        &self,
        workflow: WorkflowId,
        report: RunReport,
    ) -> Result<Transition, Error> {
        let previous = self.get(workflow).await?;
        let failures_before = previous
            .as_ref()
            .map_or(0, |state| state.consecutive_failures);
        let failed = report.outcome == RunOutcome::Failed;

        let consecutive_failures = if failed {
            failures_before.saturating_add(1)
        } else {
            0
        };

        let last_failure = if failed {
            Some(report.clone())
        } else {
            previous.and_then(|state| state.last_failure)
        };

        self.services
            .kv()
            .set(
                RUNS_PARTITION,
                workflow.to_string(),
                RunState {
                    last: report,
                    last_failure,
                    consecutive_failures,
                },
            )
            .await?;

        Ok(match (failures_before > 0, failed) {
            (false, true) => Transition::StartedFailing,
            (true, false) => Transition::Recovered {
                after: failures_before,
            },
            _ => Transition::Unchanged,
        })
    }

    /// Forgets a workflow's runs, for when the workflow itself has gone.
    pub async fn forget(&self, workflow: WorkflowId) -> Result<(), Error> {
        self.services
            .kv()
            .remove(RUNS_PARTITION, workflow.to_string())
            .await
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::services::ServicesContainer;

    fn report(outcome: RunOutcome, input: Option<Value>) -> RunReport {
        let now = Utc::now();
        RunReport {
            started_at: now,
            finished_at: now,
            outcome,
            message: (outcome == RunOutcome::Failed).then(|| "it broke".to_string()),
            input,
        }
    }

    #[test]
    fn a_signature_never_reaches_the_record() {
        // The record lands in a store the admin UI browses, so a delivery's
        // signing header would be a credential published to every operator.
        let kept = keepable(&json!({
            "event": {
                "headers": {
                    "x-hub-signature-256": "sha256=deadbeef",
                    "authorization": "Bearer hunter2",
                    "x-github-event": "push",
                },
                "body": "{\"action\":\"opened\"}",
            }
        }))
        .unwrap();

        let headers = &kept["event"]["headers"];
        assert_eq!(headers["x-hub-signature-256"], json!(REDACTED));
        assert_eq!(headers["authorization"], json!(REDACTED));
        assert_eq!(
            headers["x-github-event"],
            json!("push"),
            "redaction must not take the fields that make a delivery identifiable",
        );
    }

    #[test]
    fn an_oversized_delivery_is_kept_as_a_prefix() {
        let kept = keepable(&json!({ "body": "x".repeat(MAX_INPUT_BYTES * 2) })).unwrap();

        assert_eq!(kept["truncated"], json!(true));
        assert!(
            kept["preview"].as_str().unwrap().len() <= PREVIEW_BYTES,
            "an input kept whole would put a megabyte payload in every list read",
        );
    }

    #[tokio::test]
    async fn a_workflow_that_keeps_working_reports_no_transition() {
        let services = ServicesContainer::new_mock().await.unwrap();
        let store = RunStore::new(&services);
        let id = WorkflowId::from_entropy(1);

        store
            .record(id, report(RunOutcome::Succeeded, None))
            .await
            .unwrap();
        let second = store
            .record(id, report(RunOutcome::Succeeded, None))
            .await
            .unwrap();

        assert_eq!(second, Transition::Unchanged);
    }

    #[tokio::test]
    async fn only_the_first_failure_and_the_recovery_are_transitions() {
        // This is what bounds the audit log: an incident is two entries however
        // many runs it spans.
        let services = ServicesContainer::new_mock().await.unwrap();
        let store = RunStore::new(&services);
        let id = WorkflowId::from_entropy(2);

        store
            .record(id, report(RunOutcome::Succeeded, None))
            .await
            .unwrap();

        let first = store
            .record(id, report(RunOutcome::Failed, None))
            .await
            .unwrap();
        assert_eq!(first, Transition::StartedFailing);

        for _ in 0..5 {
            assert_eq!(
                store
                    .record(id, report(RunOutcome::Failed, None))
                    .await
                    .unwrap(),
                Transition::Unchanged,
            );
        }

        assert_eq!(
            store
                .record(id, report(RunOutcome::Succeeded, None))
                .await
                .unwrap(),
            Transition::Recovered { after: 6 },
        );
    }

    #[tokio::test]
    async fn a_failure_survives_the_runs_that_follow_it() {
        // The case the whole record exists for: a workflow that failed
        // overnight and has since run hundreds of times successfully.
        let services = ServicesContainer::new_mock().await.unwrap();
        let store = RunStore::new(&services);
        let id = WorkflowId::from_entropy(3);

        store
            .record(id, report(RunOutcome::Failed, Some(json!({ "n": 1 }))))
            .await
            .unwrap();

        for n in 2..10 {
            store
                .record(id, report(RunOutcome::Succeeded, Some(json!({ "n": n }))))
                .await
                .unwrap();
        }

        let state = store.get(id).await.unwrap().unwrap();
        assert!(!state.is_failing());
        assert_eq!(state.last.input, Some(json!({ "n": 9 })));
        assert_eq!(
            state.last_failure.unwrap().input,
            Some(json!({ "n": 1 })),
            "the payload that failed is the one worth keeping",
        );
    }

    #[tokio::test]
    async fn health_is_reported_without_the_payloads() {
        let services = ServicesContainer::new_mock().await.unwrap();
        let store = RunStore::new(&services);
        let id = WorkflowId::from_entropy(4);

        store
            .record(
                id,
                report(RunOutcome::Failed, Some(json!({ "body": "x".repeat(512) }))),
            )
            .await
            .unwrap();

        let health = store.health().await.unwrap();
        let reported = health.get(&id).unwrap();

        assert_eq!(reported.outcome, RunOutcome::Failed);
        assert_eq!(reported.consecutive_failures, 1);
        assert!(
            serde_json::to_string(reported).unwrap().len() < 512,
            "health carries no payload, or a list of workflows carries all of them",
        );
    }
}
