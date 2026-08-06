//! What a workflow last did.
//!
//! A run used to be an audit entry, one per run. That works for a feed polled
//! every six hours and not at all for a GitHub App on a busy organisation,
//! which produces thousands of deliveries a day and buries the handful of
//! entries somebody actually wanted to read.
//!
//! So a run is no longer history. Each workflow keeps one record of how it is
//! getting on, overwritten in place, and the audit log hears about a run only
//! when the answer changes.

use serde::{Deserialize, Serialize};

/// How a run turned out.
///
/// Only two answers, because the record exists to say whether the workflow is
/// working. A run that found nothing to do worked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunOutcome {
    Succeeded,
    Failed,
}

impl RunOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

/// One run, with enough of what it ran on to work out why it went the way it
/// did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: chrono::DateTime<chrono::Utc>,
    pub outcome: RunOutcome,

    /// Why it failed, in the words the failure was reported in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// What the workflow was handed: the delivery a webhook arrived with, or
    /// the configuration a schedule ran against.
    ///
    /// Redacted and size-capped before it is stored, because it is somebody
    /// else's data sitting in a store the admin UI can browse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
}

/// Everything kept about one workflow's runs.
///
/// Three fields rather than a list, so that a workflow which runs a thousand
/// times a day costs the same as one which runs twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    /// The most recent run, whatever became of it.
    pub last: RunReport,

    /// The most recent run that failed, kept even once later runs have worked.
    ///
    /// Without this, an overnight failure is gone by the morning on any
    /// workflow busy enough to have run again — which is exactly the workflow
    /// whose failures are hardest to catch in the act.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<RunReport>,

    /// How many runs have failed in a row. Zeroed by a run that works.
    #[serde(default)]
    pub consecutive_failures: u32,
}

impl RunState {
    /// Whether the workflow's last run failed.
    pub fn is_failing(&self) -> bool {
        self.consecutive_failures > 0
    }

    /// The summary carried on a workflow in list responses.
    ///
    /// Deliberately drops the inputs. A list of workflows would otherwise carry
    /// every one of their payloads, which is the same saturation problem in a
    /// different place.
    pub fn health(&self) -> WorkflowHealth {
        WorkflowHealth {
            outcome: self.last.outcome,
            at: self.last.finished_at,
            message: self.last.message.clone(),
            consecutive_failures: self.consecutive_failures,
            last_failure_at: self
                .last_failure
                .as_ref()
                .map(|failure| failure.finished_at),
        }
    }
}

/// How a workflow is getting on, as shown beside it in a list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowHealth {
    pub outcome: RunOutcome,

    /// When the most recent run finished.
    pub at: chrono::DateTime<chrono::Utc>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    #[serde(default)]
    pub consecutive_failures: u32,

    /// When the most recent failure was, which may be older than the last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<chrono::DateTime<chrono::Utc>>,
}
