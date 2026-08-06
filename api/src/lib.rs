//! Shared data-transfer types for the Automate REST API.
//!
//! This crate is deliberately free of any web-framework, database, or UI
//! dependencies so that it can be compiled both by the `automate` agent
//! (native) and the `automate-ui` crate (WebAssembly). It defines the JSON
//! contract exchanged over the `/api/v1` endpoints.

mod audit;
mod connection;
pub mod ids;
mod integration;
mod kv;
mod queue;
mod run;
mod tenant;
mod user;
mod webhook;
mod wordlist;
mod workflow;

pub use audit::{AuditCategory, AuditOutcome, AuditRecord};
pub use connection::{ConnectionKind, ConnectionStatus, ConnectionSummary, OptionItem};
pub use ids::{ConnectionId, WordId, WordIdError, WorkflowId};
pub use integration::{Connection, IntegrationInfo};
pub use kv::KeyValueEntry;
pub use queue::{QueueMessage, QueueStatus};
pub use run::{RunOutcome, RunReport, RunState, WorkflowHealth};
pub use tenant::{TenantId, TenantIdError};
pub use user::AdminUser;
pub use webhook::{WebhookToken, WebhookTokenError};
pub use workflow::{FieldDescriptor, FieldKind, Workflow, WorkflowTrigger, WorkflowTypeDescriptor};
