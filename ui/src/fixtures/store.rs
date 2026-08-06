//! A mutable, in-memory stand-in for the agent.
//!
//! Demo mode is more useful if it behaves like the application rather than like
//! a screenshot: pausing a workflow, linking an account, or deleting a queued
//! message all have to stick, or the pages that exist to do those things cannot
//! be reviewed at all. So the fixtures are loaded into a store which the demo
//! branches in [`crate::api`] read and write, and which lives for as long as the
//! tab does.
//!
//! It is single-threaded because WebAssembly is, and it forgets everything on
//! reload because a demo that accumulates state is a demo that stops being
//! reproducible.

use std::cell::RefCell;

use automate_api::{
    AdminUser, AuditRecord, Connection, ConnectionId, ConnectionKind, ConnectionStatus,
    ConnectionSummary, FieldKind, IntegrationInfo, KeyValueEntry, OptionItem, QueueMessage,
    QueueStatus, RunState, Workflow, WorkflowId, WorkflowTrigger, WorkflowTypeDescriptor,
};
use chrono::Utc;

use super::data;
use crate::components::dynamic_form::value_at;

struct State {
    kv: Vec<KeyValueEntry>,
    queue: Vec<QueueMessage>,
    connections: Vec<ConnectionSummary>,
    workflows: Vec<Workflow>,
    integration_connections: Vec<(String, Vec<Connection>)>,
    /// Distinguishes the records created during this session from the fixtures
    /// and from each other.
    next_id: u64,
}

impl State {
    fn new() -> Self {
        Self {
            kv: data::kv_entries(),
            queue: data::queue_messages(),
            connections: data::service_connections(),
            workflows: data::workflows(),
            integration_connections: data::integrations()
                .into_iter()
                .map(|integration| {
                    let connections = data::integration_connections(&integration.id);
                    (integration.id, connections)
                })
                .collect(),
            next_id: 100,
        }
    }

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

fn with<R>(action: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| action(&mut state.borrow_mut()))
}

pub fn admin_user() -> AdminUser {
    data::admin_user()
}

pub fn kv_entries() -> Vec<KeyValueEntry> {
    with(|state| state.kv.clone())
}

pub fn delete_kv(partition: &str, key: &str) {
    with(|state| {
        state
            .kv
            .retain(|entry| entry.partition != partition || entry.key != key)
    });
}

pub fn queue_messages() -> Vec<QueueMessage> {
    with(|state| state.queue.clone())
}

pub fn delete_queue(partition: &str, key: &str) {
    with(|state| {
        state
            .queue
            .retain(|message| message.partition != partition || message.key != key)
    });
}

/// Re-enqueues a message, which is what the agent's trigger endpoint does: the
/// message becomes available now rather than whenever it was hidden until.
pub fn trigger_queue(partition: &str, key: &str, payload: serde_json::Value) {
    with(|state| {
        if let Some(message) = state
            .queue
            .iter_mut()
            .find(|message| message.partition == partition && message.key == key)
        {
            message.payload = payload;
            message.status = QueueStatus::Pending;
            message.scheduled_at = Utc::now();
            message.hidden_until = None;
        }
    });
}

pub fn service_connections() -> Vec<ConnectionSummary> {
    with(|state| state.connections.clone())
}

pub fn create_service_connection(provider: &str, name: &str) -> ConnectionSummary {
    with(|state| {
        let now = Utc::now();
        let connection = ConnectionSummary {
            id: ConnectionId::from_entropy(state.take_id()),
            provider: provider.to_string(),
            kind: ConnectionKind::ApiKey,
            name: name.to_string(),
            account: None,
            status: ConnectionStatus::Ok,
            expires_at: None,
            metadata: Default::default(),
            created_at: now,
            updated_at: now,
        };

        state.connections.push(connection.clone());
        connection
    })
}

pub fn update_service_connection(
    id: &str,
    name: &str,
    key: Option<&str>,
) -> Option<ConnectionSummary> {
    with(|state| {
        let connection = state
            .connections
            .iter_mut()
            .find(|connection| connection.id.to_string() == id)?;

        connection.name = name.to_string();
        if key.is_some_and(|key| !key.trim().is_empty()) {
            connection.status = ConnectionStatus::Ok;
        }
        connection.updated_at = Utc::now();
        Some(connection.clone())
    })
}

pub fn delete_service_connection(id: &str) {
    with(|state| {
        state
            .connections
            .retain(|connection| connection.id.to_string() != id)
    });
}

pub fn workflow_types() -> Vec<WorkflowTypeDescriptor> {
    data::workflow_types()
}

pub fn workflows() -> Vec<Workflow> {
    with(|state| state.workflows.clone())
}

/// How a workflow's most recent runs went, payloads included.
pub fn workflow_runs(workflow: &str) -> Option<RunState> {
    data::workflow_runs(workflow)
}

/// The audit log, narrowed the way the agent narrows it.
///
/// The filtering is repeated here rather than left to the page because the page
/// does not do it: it asks for what it wants and expects the answer to already
/// be that. A demo store that returned everything would let a page ship with a
/// filter it never actually applies.
pub fn audit(subject: Option<&str>, before: Option<i64>) -> Vec<AuditRecord> {
    data::audit()
        .into_iter()
        .filter(|entry| match subject {
            Some(subject) => entry.subject.as_deref() == Some(subject),
            None => true,
        })
        .filter(|entry| match before {
            Some(before) => entry.id < before,
            None => true,
        })
        .collect()
}

pub fn create_workflow(
    type_id: &str,
    config: &serde_json::Value,
    schedule: Option<&str>,
    enabled: bool,
) -> Option<Workflow> {
    let descriptor = data::workflow_types()
        .into_iter()
        .find(|descriptor| descriptor.id == type_id)?;

    with(|state| {
        let now = Utc::now();
        let id = state.take_id();
        let workflow = Workflow {
            id: WorkflowId::from_entropy(id),
            type_id: descriptor.id.clone(),
            name: derive_name(&descriptor, config),
            enabled,
            config: config.clone(),
            schedule: schedule.map(str::to_string),
            webhook_path: webhook_path(&descriptor.trigger, id),
            // The agent derives this from the type's declared state. Here the
            // trigger is the closest honest stand-in: the workflows that poll
            // are the ones that remember where they got to.
            resettable: matches!(descriptor.trigger, WorkflowTrigger::Cron { .. }),
            created_at: now,
            updated_at: now,
            last_run: None,
            next_run: None,
            // Nothing has run it yet, which is what a workflow created a moment
            // ago should look like.
            health: None,
        };

        state.workflows.push(workflow.clone());
        Some(workflow)
    })
}

pub fn update_workflow(
    id: &str,
    config: &serde_json::Value,
    schedule: Option<&str>,
    enabled: bool,
) -> Option<Workflow> {
    let types = data::workflow_types();

    with(|state| {
        let workflow = state
            .workflows
            .iter_mut()
            .find(|workflow| workflow.id.to_string() == id)?;

        if let Some(descriptor) = types
            .iter()
            .find(|descriptor| descriptor.id == workflow.type_id)
        {
            workflow.name = derive_name(descriptor, config);
        }

        workflow.config = config.clone();
        workflow.schedule = schedule.map(str::to_string);
        workflow.enabled = enabled;
        workflow.updated_at = Utc::now();
        Some(workflow.clone())
    })
}

pub fn rotate_webhook(id: &str) -> Option<Workflow> {
    with(|state| {
        let token = token(state.take_id());
        let workflow = state
            .workflows
            .iter_mut()
            .find(|workflow| workflow.id.to_string() == id)?;

        // Only the token changes; the source segment identifies the provider and
        // is not the workflow's to reissue.
        if let Some(path) = &workflow.webhook_path {
            let prefix = path.rsplit_once('/').map(|(head, _)| head).unwrap_or(path);
            workflow.webhook_path = Some(format!("{prefix}/{token}"));
        }

        workflow.updated_at = Utc::now();
        Some(workflow.clone())
    })
}

pub fn delete_workflow(id: &str) {
    with(|state| {
        state
            .workflows
            .retain(|workflow| workflow.id.to_string() != id)
    });
}

/// Runs a workflow now. There is no job host here to run it, so all this can
/// honestly do is record that it was asked for.
pub fn trigger_workflow(id: &str) -> Option<()> {
    with(|state| {
        let workflow = state
            .workflows
            .iter_mut()
            .find(|workflow| workflow.id.to_string() == id)?;

        workflow.last_run = Some(Utc::now());
        Some(())
    })
}

/// Forgets what a workflow remembers between runs.
///
/// The agent works out which stored values belong to a workflow from its type
/// and configuration, which is knowledge the browser deliberately does not have.
/// So this reports the shape of the answer — how many values were cleared —
/// without pretending to know which entries in the demo's data view they were.
pub fn reset_workflow(id: &str) -> Option<usize> {
    with(|state| {
        let workflow = state
            .workflows
            .iter()
            .find(|workflow| workflow.id.to_string() == id)?;

        workflow.resettable.then_some(1)
    })
}

pub fn connection_options(source: &str, parent: Option<&str>) -> Vec<OptionItem> {
    data::connection_options(source, parent)
}

pub fn integrations() -> Vec<IntegrationInfo> {
    data::integrations()
}

pub fn integration_connections(integration: &str) -> Vec<Connection> {
    with(|state| {
        state
            .integration_connections
            .iter()
            .find(|(id, _)| id == integration)
            .map(|(_, connections)| connections.clone())
            .unwrap_or_default()
    })
}

pub fn disconnect(integration: &str, connection: &str) {
    with(|state| {
        if let Some((_, connections)) = state
            .integration_connections
            .iter_mut()
            .find(|(id, _)| id == integration)
        {
            connections.retain(|candidate| candidate.id != connection);
        }
    });
}

/// The name a workflow gets from its configuration.
///
/// The first text-like field carrying a value is the closest thing a
/// configuration has to a title — a feed's address, a label's name — and a type
/// with nothing to draw on falls back to its own name, which mirrors how the
/// agent derives one.
fn derive_name(descriptor: &WorkflowTypeDescriptor, config: &serde_json::Value) -> String {
    descriptor
        .fields
        .iter()
        .filter(|field| matches!(field.kind, FieldKind::Text { .. } | FieldKind::Url { .. }))
        .find_map(|field| match value_at(config, &field.name) {
            Some(serde_json::Value::String(text)) if !text.trim().is_empty() => {
                Some(text.trim().to_string())
            }
            _ => None,
        })
        .unwrap_or_else(|| descriptor.name.clone())
}

/// The address a webhook-triggered workflow is reached at, for the triggers that
/// have one of their own. A routed webhook shares its provider's endpoint and so
/// has no address to show.
fn webhook_path(trigger: &WorkflowTrigger, id: u64) -> Option<String> {
    match trigger {
        WorkflowTrigger::Webhook { source } => Some(format!("/webhooks/{source}/{}", token(id))),
        WorkflowTrigger::Cron { .. } | WorkflowTrigger::RoutedWebhook { .. } => None,
    }
}

/// A stand-in for the high-entropy token a real ingress URL carries. It only has
/// to look like one and differ from the last, which multiplying by an odd
/// constant achieves without pulling in a generator.
fn token(id: u64) -> String {
    format!("{:032x}", id.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}
