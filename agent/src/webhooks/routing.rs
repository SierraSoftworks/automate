//! Routing for the webhooks a service posts to one shared address.
//!
//! There are two kinds of webhook in this agent. A workflow-addressed one has
//! its own secret URL, so the delivery names the workflow directly and
//! [`crate::web::webhooks::deliver`] can hand it straight over. A *shared* one
//! — a GitHub App, a Todoist app — has one address for the whole installation,
//! configured once by the operator, and every delivery for every user arrives
//! there. Working out whose it is, and which of their workflows asked for it,
//! is the job this module does.
//!
//! That work is the same for every such service: check the signature, decide
//! which account the delivery names, find that account's connections across the
//! tenants, then queue one copy per enabled workflow that selected one of them.
//! Written out per service it is around a hundred and fifty lines of identical
//! control flow with four service-specific expressions buried in it, which is
//! how [`crate::web::webhooks`] grew a near-duplicate function per provider.
//!
//! So the loop lives here and each service implements [`WebhookSource`], which
//! is only those four expressions: what it signs with, how it signs, which
//! account a delivery names, and which of that account's connections it belongs
//! to. Adding a service is then a `WebhookSource` impl next to that service's
//! webhook type — no new route, no new handler, and nothing to keep in step
//! with the others.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use automate_api::{ConnectionId, TenantId};

use super::{WebhookDelivery, WebhookEvent};
use crate::connections::ConnectionStore;
use crate::db::{KeyValueStore, Queue};
use crate::prelude::*;
use crate::services::{AppContext, AppServices};
use crate::workflow_store::WorkflowRecord;

/// One service that posts every user's deliveries to a single address.
///
/// Concrete over [`AppServices`] rather than generic over [`Services`], for the
/// same reason [`crate::integrations::Integration`] is: the registry has to name
/// one services type to stay object-safe.
#[async_trait::async_trait]
pub trait WebhookSource: Send + Sync {
    /// The path segment this source is served at, as in `/webhooks/{id}`.
    ///
    /// Also the `source` a workflow type advertises in
    /// [`automate_api::WorkflowTrigger::RoutedWebhook`], so the address the
    /// documentation tells somebody to configure is the one that reaches them.
    fn id(&self) -> &'static str;

    /// The workflow type deliveries are dispatched to.
    ///
    /// The queue partition is taken from this rather than declared separately,
    /// so the two cannot drift apart.
    fn workflow(&self) -> &'static str;

    /// The secret deliveries are signed with, or `None` when this service is
    /// not configured on this instance.
    fn secret(&self, config: &Config) -> Option<String>;

    /// Checks the signature over the raw body, exactly as the sender computed
    /// it.
    fn verify(&self, secret: &str, event: &WebhookEvent) -> Result<(), human_errors::Error>;

    /// The header naming a delivery, held constant across the sender's own
    /// retries, and therefore usable as an idempotency key.
    fn delivery_header(&self) -> &'static str;

    /// Which account at the service this delivery belongs to.
    ///
    /// `None` means there is nothing to route — a ping, or an event the service
    /// sends that is not about any one account — and is answered with a success
    /// rather than an error, since the sender did nothing wrong.
    fn account(&self, event: &WebhookEvent, payload: &serde_json::Value) -> Option<String>;

    /// The connections in this tenant that `account` refers to.
    ///
    /// Kept here rather than shared because services identify an account
    /// differently: Todoist names a user, so the connection records it; a GitHub
    /// App names an installation, which is inside the credential.
    async fn connections(
        &self,
        account: &str,
        store: &ConnectionStore<AppServices>,
    ) -> Result<HashSet<ConnectionId>, human_errors::Error>;

    /// The connection a stored workflow selected, if it named one.
    fn selects(&self, config: &serde_json::Value) -> Option<ConnectionId>;

    /// Anything to do besides queueing, once the connections are known.
    ///
    /// Runs whether or not any workflow matched, because it is about the
    /// account rather than about the work — invalidating what we cached from the
    /// service, say. A failure is logged and does not fail the delivery.
    async fn observe(
        &self,
        _payload: &serde_json::Value,
        _connections: &HashSet<ConnectionId>,
        _services: &AppServices,
    ) -> Result<(), human_errors::Error> {
        Ok(())
    }
}

/// A registration entry for a [`WebhookSource`], collected by [`inventory`].
pub struct WebhookSourceRegistration(&'static dyn WebhookSource);

impl WebhookSourceRegistration {
    pub const fn new<T: WebhookSource>(source: &'static T) -> Self {
        Self(source)
    }

    pub fn source(&self) -> &'static dyn WebhookSource {
        self.0
    }
}

inventory::collect!(WebhookSourceRegistration);

/// Registers a [`WebhookSource`] so that `/webhooks/{id}` reaches it.
#[macro_export]
macro_rules! register_webhook_source {
    ($source:expr) => {
        inventory::submit! { $crate::webhooks::WebhookSourceRegistration::new(&$source) }
    };
}

/// Every registered source, keyed by the path segment it answers at.
///
/// A duplicate identifier would silently shadow one service's endpoint with
/// another's, which is a delivery quietly going to the wrong place, so it panics
/// here rather than being discovered in production.
fn registry() -> &'static HashMap<&'static str, &'static dyn WebhookSource> {
    static REGISTRY: LazyLock<HashMap<&'static str, &'static dyn WebhookSource>> =
        LazyLock::new(|| {
            let mut registry: HashMap<&'static str, &'static dyn WebhookSource> = HashMap::new();

            for registration in inventory::iter::<WebhookSourceRegistration> {
                let source = registration.source();
                if registry.insert(source.id(), source).is_some() {
                    panic!(
                        "Two webhook sources are registered as '{}'. Each needs its own address.",
                        source.id()
                    );
                }
            }

            registry
        });

    &REGISTRY
}

/// The source served at `/webhooks/{id}`, if there is one.
pub fn source(id: &str) -> Option<&'static dyn WebhookSource> {
    registry().get(id).copied()
}

/// What became of a delivery, in the terms the endpoint answers in.
///
/// An enum rather than a response so that the routing is testable without an
/// HTTP stack, and so the one place that maps these to status codes is the one
/// place they can be got wrong.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivered {
    /// Accepted. Also the answer when nothing matched: the sender did nothing
    /// wrong, and a service that treats a non-2xx as a failure would otherwise
    /// disable a webhook over deliveries we simply had no workflow for.
    Accepted,

    /// This service is not configured on this instance.
    Unavailable,

    /// The signature was missing or did not match.
    Unauthorized,

    /// The body was not the JSON the service is documented to send.
    Malformed,

    /// Something on our side failed, and the sender should retry.
    Failed,
}

/// Routes one delivery to every workflow that asked for it.
///
/// The body has already been read and length-limited by the endpoint, because
/// that needs the HTTP stack; everything after it is here.
#[instrument("webhooks.route", skip(source, context, event), fields(webhook.source = source.id()))]
pub async fn route(
    source: &'static dyn WebhookSource,
    context: &AppContext,
    event: WebhookEvent,
) -> Delivered {
    let Some(secret) = source.secret(&context.config()) else {
        warn!(
            "Received a {} webhook, but that service is not configured here.",
            source.id()
        );
        return Delivered::Unavailable;
    };

    if let Err(err) = source.verify(&secret, &event) {
        warn!(error = %err, "Rejected a {} webhook: {err}", source.id());
        return Delivered::Unauthorized;
    }

    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.body) else {
        warn!(
            "Rejected a {} webhook whose body was not JSON.",
            source.id()
        );
        return Delivered::Malformed;
    };

    let Some(account) = source.account(&event, &payload) else {
        debug!(
            "Ignoring a {} delivery that does not name an account.",
            source.id()
        );
        return Delivered::Accepted;
    };

    let partition = match crate::workflows::lookup(source.workflow()) {
        Ok(workflow) => workflow.partition(),
        Err(err) => {
            error!(error = %err, "A webhook source names a workflow type we do not have: {err}");
            return Delivered::Failed;
        }
    };

    let tenants = match context.database().tenants().await {
        Ok(tenants) => tenants,
        Err(err) => {
            error!(error = %err, "Failed to enumerate tenants for a webhook: {err}");
            return Delivered::Failed;
        }
    };

    let delivery = event.header(source.delivery_header()).map(str::to_string);

    for tenant in tenants {
        // Nobody's connections live here; it holds the installation's own
        // records.
        if tenant == TenantId::system() {
            continue;
        }

        let services = context.tenant(tenant);
        let store = ConnectionStore::for_services(services.clone());

        let matching = match source.connections(&account, &store).await {
            Ok(matching) => matching,
            Err(err) => {
                error!(error = %err, "Failed to load connections while routing a webhook: {err}");
                return Delivered::Failed;
            }
        };

        if matching.is_empty() {
            continue;
        }

        if let Err(err) = source.observe(&payload, &matching, &services).await {
            // The delivery is still worth routing; whatever this was doing is
            // by definition something other than the work.
            warn!(error = %err, "A webhook source failed to act on a delivery: {err}");
        }

        let workflows: Vec<(String, WorkflowRecord)> = match services.kv().list(partition).await {
            Ok(workflows) => workflows,
            Err(err) => {
                error!(error = %err, "Failed to load workflows while routing a webhook: {err}");
                return Delivered::Failed;
            }
        };

        for (_, workflow) in workflows {
            if !workflow.enabled || workflow.type_id != source.workflow() {
                continue;
            }

            let Some(selected) = source.selects(&workflow.config) else {
                continue;
            };

            if !matching.contains(&selected) {
                continue;
            }

            let idempotency_key = delivery
                .as_ref()
                .map(|delivery| Cow::Owned(format!("{delivery}/{}", workflow.id)));

            if let Err(err) = services
                .queue()
                .enqueue(
                    partition,
                    WebhookDelivery {
                        workflow: workflow.id,
                        event: event.clone(),
                    },
                    idempotency_key,
                    None,
                )
                .await
            {
                error!(error = %err, workflow.id = %workflow.id, "Failed to enqueue a webhook delivery: {err}");
                return Delivered::Failed;
            }
        }
    }

    Delivered::Accepted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address a workflow's documentation tells somebody to configure has to
    /// be one that reaches it. These are two independent registrations — the
    /// workflow type and the source — and nothing else would notice them
    /// disagreeing until deliveries started arriving at a 404.
    #[test]
    fn every_routed_workflow_type_has_an_address_that_reaches_it() {
        for descriptor in crate::workflows::descriptors() {
            let automate_api::WorkflowTrigger::RoutedWebhook { source: id } = &descriptor.trigger
            else {
                continue;
            };

            let source = source(id).unwrap_or_else(|| {
                panic!(
                    "The '{}' workflow tells people to configure /webhooks/{id}, but nothing is registered there.",
                    descriptor.id
                )
            });

            assert_eq!(
                source.workflow(),
                descriptor.id,
                "/webhooks/{id} dispatches somewhere other than the workflow that advertises it",
            );
        }
    }

    /// A source with nothing to dispatch to would accept deliveries and drop
    /// every one of them.
    #[test]
    fn every_source_dispatches_to_a_workflow_type_that_exists() {
        for registration in inventory::iter::<WebhookSourceRegistration> {
            let source = registration.source();
            assert!(
                crate::workflows::lookup(source.workflow()).is_ok(),
                "/webhooks/{} dispatches to '{}', which is not a workflow type",
                source.id(),
                source.workflow(),
            );
        }
    }
}
