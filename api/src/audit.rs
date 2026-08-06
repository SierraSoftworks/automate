//! The audit log's wire contract.
//!
//! These live here rather than beside the agent's storage because the log is
//! read by the admin UI as well as written by the agent, and a second
//! definition of the same values is a second place for them to drift.

use serde::{Deserialize, Serialize};

use crate::TenantId;

/// The area of the system an entry concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditCategory {
    /// A workflow ran.
    WorkflowRun,

    /// A webhook delivery was received and processed.
    WebhookDelivery,

    /// A workflow was created, changed or removed.
    WorkflowConfig,

    /// A connection to an external service was established or removed.
    Connection,

    /// Someone signed in, or was refused.
    Authentication,

    /// An administrator acted, including impersonating a user.
    Administration,
}

impl AuditCategory {
    /// Every category, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[
        Self::WorkflowRun,
        Self::WebhookDelivery,
        Self::WorkflowConfig,
        Self::Connection,
        Self::Authentication,
        Self::Administration,
    ];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WorkflowRun => "workflow.run",
            Self::WebhookDelivery => "webhook.delivery",
            Self::WorkflowConfig => "workflow.config",
            Self::Connection => "connection",
            Self::Authentication => "authentication",
            Self::Administration => "administration",
        }
    }

    /// A short phrase naming the category for somebody reading their own log.
    pub fn label(&self) -> &'static str {
        match self {
            Self::WorkflowRun => "Workflow run",
            Self::WebhookDelivery => "Webhook delivery",
            Self::WorkflowConfig => "Workflow change",
            Self::Connection => "Connection",
            Self::Authentication => "Sign-in",
            Self::Administration => "Administration",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "workflow.run" => Self::WorkflowRun,
            "webhook.delivery" => Self::WebhookDelivery,
            "workflow.config" => Self::WorkflowConfig,
            "connection" => Self::Connection,
            "authentication" => Self::Authentication,
            "administration" => Self::Administration,
            _ => return None,
        })
    }
}

/// How an audited operation turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditOutcome {
    /// The operation did what it set out to do.
    Success,

    /// The operation was attempted and did not work.
    Failure,

    /// The operation was deliberately not performed, because a filter excluded
    /// it or there was nothing to do.
    Skipped,

    /// The operation was refused: a bad signature, or a permission check.
    Denied,
}

impl AuditOutcome {
    /// Every outcome, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::Success, Self::Failure, Self::Skipped, Self::Denied];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Skipped => "skipped",
            Self::Denied => "denied",
        }
    }

    /// A short phrase naming the outcome for somebody reading their own log.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Success => "Succeeded",
            Self::Failure => "Failed",
            Self::Skipped => "Skipped",
            Self::Denied => "Refused",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "success" => Self::Success,
            "failure" => Self::Failure,
            "skipped" => Self::Skipped,
            "denied" => Self::Denied,
            _ => return None,
        })
    }
}

/// An entry read back from the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Monotonic identifier, which is also the pagination cursor.
    pub id: i64,

    pub tenant: TenantId,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub category: AuditCategory,
    pub action: String,
    pub outcome: AuditOutcome,

    /// The thing acted upon: a workflow or connection identifier, or a username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,

    /// The person responsible, where there was one. Absent for work the agent
    /// did on its own initiative, which is how a scheduled run is told apart
    /// from one somebody asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}
