//! Workflows driven by what happens in somebody's Todoist account.
//!
//! Every other webhook in this agent brings work *into* Todoist. This one goes
//! the other way: Todoist reports a task being completed, a project being
//! renamed, a comment being added, and a workflow decides what that is worth.
//!
//! # Why the payload is not modelled
//!
//! Todoist's event catalogue is wide — items, notes, projects, sections,
//! labels, filters and reminders, each with several verbs — and `event_data`
//! carries a different object for each. Modelling all of them would be a large
//! amount of code that goes stale the moment Todoist adds a verb, and would
//! still leave somebody unable to match on a field we had not thought to
//! include. So the delivery is handed to the user's filter and templates as
//! JSON, addressed by path, exactly as [`crate::jobs::WebhookTodoistWorkflow`]
//! does for senders we have never heard of.
//!
//! What this workflow adds over that generic one is routing: the deliveries it
//! sees are the ones Todoist sent for the account it names, verified against the
//! application's own signature, rather than anything that found an address.
//!
//! # Loops
//!
//! A workflow that files a Todoist task in response to a Todoist event can feed
//! itself: the task it creates is an `item:added` event, which arrives here, and
//! creates another. The filter is what breaks that, so the documentation is
//! blunt about it and the default filter names a single event rather than
//! matching everything.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Display;

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use automate_api::ConnectionId;

use crate::connections::ConnectionStore;
use crate::prelude::*;
use crate::publishers::{
    TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate, TodoistTarget,
};
use crate::services::AppServices;
use crate::webhook_payload::{JsonFilter, render};
use crate::webhooks::{WebhookDelivery, WebhookSource};

type HmacSha256 = Hmac<Sha256>;

/// The header Todoist signs each delivery with.
pub const SIGNATURE_HEADER: &str = "x-todoist-hmac-sha256";

/// The header naming a delivery, held constant across Todoist's own retries.
pub const DELIVERY_HEADER: &str = "x-todoist-delivery-id";

#[derive(Clone, Serialize, Deserialize)]
pub struct TodoistWebhookConfig {
    /// What to call this workflow, so it can be told apart in a list.
    pub name: String,

    /// Whose Todoist account this workflow watches.
    ///
    /// Required, and the reason this workflow can be routed at all: a delivery
    /// names the Todoist user it happened to, and this says which of the linked
    /// accounts that is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<ConnectionId>,

    /// Which events are worth acting on. Matched against the delivery by path.
    #[serde(default)]
    pub filter: Filter,

    /// The task's title, rendered against the delivery.
    pub title: String,

    /// The task's body, rendered the same way.
    #[serde(default)]
    pub description: Option<String>,

    /// Where the resulting task is filed. Not necessarily the account the event
    /// came from — filing a follow-up into a shared account is a reasonable
    /// thing to want.
    #[serde(default)]
    pub todoist: TodoistTarget,
}

impl Display for TodoistWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "todoist/{}", self.name)
    }
}

#[derive(Clone)]
pub struct TodoistWebhook;

impl TodoistWebhook {
    /// Verifies the `X-Todoist-Hmac-SHA256` header, which Todoist populates
    /// with the base64-encoded HMAC-SHA256 of the raw request body, keyed with
    /// the app's Client Secret.
    ///
    /// Note the two differences from GitHub's otherwise identical scheme: the
    /// digest is base64 rather than hex, and it is unprefixed. Both are easy to
    /// get subtly right against one provider's deliveries and wrong against
    /// another's, which is why this is verified against known vectors in the
    /// tests below rather than only end to end.
    pub fn verify_signature(
        secret: &str,
        body: &str,
        signature_header: &str,
    ) -> Result<(), human_errors::Error> {
        let expected = base64::engine::general_purpose::STANDARD
            .decode(signature_header.trim())
            .or_user_err(&[
                "The X-Todoist-Hmac-SHA256 header is not valid base64.",
                "Ensure that only Todoist webhooks are sent to this endpoint.",
            ])?;

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).wrap_user_err(
            "Failed to create an HMAC instance with the configured client secret.",
            &["Ensure that connections.todoist.app.client_secret is configured."],
        )?;

        mac.update(body.as_bytes());

        // Constant-time, and the reason this is not a byte comparison.
        mac.verify_slice(&expected).wrap_user_err(
            "Webhook signature verification failed (signatures did not match).".to_string(),
            &[
                "Ensure that connections.todoist.app.client_secret matches the Client Secret of the app that owns this webhook.",
            ],
        )?;

        Ok(())
    }

    /// The names a delivery exposes to a filter.
    ///
    /// Only the ones every event carries. `event_data` differs per event type,
    /// so suggesting its fields would be suggesting whichever event we happened
    /// to think of; the documentation says how to find the rest instead.
    fn filter_fields() -> Vec<String> {
        [
            "event_name",
            "user_id",
            "event_data.id",
            "event_data.content",
            "event_data.description",
            "event_data.project_id",
            "event_data.section_id",
            "event_data.priority",
            "event_data.labels",
            "event_data.checked",
            "initiator.email",
            "triggered_at",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }
}

crate::register_job!(TodoistWebhook);
crate::register_workflow_type!(TodoistWebhook);
crate::register_webhook_source!(TodoistWebhook);

#[async_trait::async_trait]
impl WebhookSource for TodoistWebhook {
    fn id(&self) -> &'static str {
        "todoist"
    }

    fn workflow(&self) -> &'static str {
        <Self as crate::workflows::ConfigurableWorkflow>::type_id()
    }

    /// Todoist signs deliveries with the app's Client Secret.
    fn secret(&self, config: &Config) -> Option<String> {
        crate::integrations::todoist::app(config)
            .map(|app| app.client_secret.clone())
            .filter(|secret| !secret.is_empty())
    }

    fn verify(&self, secret: &str, event: &WebhookEvent) -> Result<(), human_errors::Error> {
        let signature = event.header(SIGNATURE_HEADER).ok_or_else(|| {
            human_errors::user(
                "The delivery did not carry an X-Todoist-Hmac-SHA256 header.",
                &["Ensure that only Todoist webhooks are sent to this endpoint."],
            )
        })?;

        Self::verify_signature(secret, &event.body, signature)
    }

    fn delivery_header(&self) -> &'static str {
        DELIVERY_HEADER
    }

    /// Todoist sends the user id as a string, but has historically sent it as a
    /// number, and the two are the same account.
    fn account(&self, _event: &WebhookEvent, payload: &serde_json::Value) -> Option<String> {
        match payload.get("user_id") {
            Some(serde_json::Value::String(id)) => Some(id.clone()),
            Some(serde_json::Value::Number(id)) => Some(id.to_string()),
            _ => None,
        }
    }

    /// Matched on the account the connection was linked to, which is the Todoist
    /// user id the authorisation named. A token imported from an older
    /// configuration file has no account and so never matches, which is right:
    /// Todoist only sends events for accounts that authorised the app.
    async fn connections(
        &self,
        account: &str,
        store: &ConnectionStore<AppServices>,
    ) -> Result<HashSet<ConnectionId>, human_errors::Error> {
        Ok(store
            .list_for_provider(crate::publishers::TODOIST_PROVIDER)
            .await?
            .into_iter()
            .filter(|connection| connection.account.as_deref() == Some(account))
            .map(|connection| connection.id)
            .collect())
    }

    fn selects(&self, config: &serde_json::Value) -> Option<ConnectionId> {
        serde_json::from_value::<TodoistWebhookConfig>(config.clone())
            .ok()
            .and_then(|config| config.connection)
    }

    /// The project and section lists are cached for a day, because resolving a
    /// name to an id on every task would be a request per task. That is fine
    /// until somebody adds the project they then try to file into, so a
    /// structural change drops the cached lists rather than leaving them to
    /// expire — the one thing a real-time feed is good for.
    async fn observe(
        &self,
        payload: &serde_json::Value,
        connections: &HashSet<ConnectionId>,
        services: &AppServices,
    ) -> Result<(), human_errors::Error> {
        let event_name = payload
            .get("event_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        if !event_name.starts_with("project:") && !event_name.starts_with("section:") {
            return Ok(());
        }

        for id in connections {
            let key = id.to_string();
            for partition in ["todoist/projects", "todoist/sections"] {
                services.kv().remove(partition, key.clone()).await?;
            }
        }

        Ok(())
    }
}

impl crate::workflows::ConfigurableWorkflow for TodoistWebhook {
    type ConfigType = TodoistWebhookConfig;

    fn type_id() -> &'static str {
        "todoist"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Todoist".to_string(),
            description:
                "Reacts to what happens in your Todoist account, as it happens rather than on a poll."
                    .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::RoutedWebhook {
                source: "todoist".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(TodoistWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Follow up on completed errands".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(TodoistWebhookConfig: connection),
                    "Todoist account to watch",
                    FieldKind::Connection {
                        provider: crate::publishers::TODOIST_PROVIDER.to_string(),
                        connection_kind: Some(automate_api::ConnectionKind::OAuth2),
                    },
                )
                .with_help(
                    "Which linked account's events this workflow handles. Only accounts connected through the Todoist app receive events; an imported API token cannot.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(TodoistWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: Self::filter_fields(),
                    },
                )
                .with_default("event_name == \"item:completed\"")
                .with_help(
                    "Which events are worth acting on. Leave this matching a single event unless you are sure: a workflow that files a task for every event will react to the task it just filed.",
                ),
                FieldDescriptor::new(
                    crate::config_path!(TodoistWebhookConfig: title),
                    "Title",
                    FieldKind::Text {
                        placeholder: Some("Review: ${{ event_data.content }}".into()),
                    },
                )
                .with_help("The task's title. Insert values from the event with ${{ path }}.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(TodoistWebhookConfig: description),
                    "Description",
                    FieldKind::TextArea {
                        placeholder: Some("Completed by ${{ initiator.email }}".into()),
                    },
                )
                .with_help("Optional. Written the same way as the title."),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                TodoistWebhookConfig,
                project = Some("Inbox"),
                section = None::<&str>
            ))
            .collect(),
        }
    }
}

impl Job for TodoistWebhook {
    type JobType = WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/todoist"
    }

    #[instrument("webhooks.todoist.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the delivery, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<TodoistWebhookConfig>(services).await? else {
            return Ok(());
        };

        // Signed and parsed before it was queued, so a body that is not JSON
        // cannot reach here; it is still a discard rather than a failure, since
        // no number of retries would make it parse.
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&job.event.body) else {
            warn!(
                workflow.id = %job.workflow,
                "Ignoring a Todoist delivery whose body is not JSON.",
            );
            return Ok(());
        };

        if !config.filter.matches(&JsonFilter(&payload))? {
            debug!(
                workflow.id = %job.workflow,
                "A Todoist event did not match this workflow's filter, so nothing was filed.",
            );
            return Ok(());
        }

        let title = render(&config.title, &payload)?;

        let description = match &config.description {
            Some(template) => Some(render(template, &payload)?),
            None => None,
        };

        // Todoist holds its delivery id constant across its own retries, which
        // makes it the natural idempotency key for the task this fans out to.
        let idempotency_key = job
            .event
            .header(DELIVERY_HEADER)
            .map(|value| Cow::Owned(format!("{value}/{}", job.workflow)));

        TodoistCreateTask::dispatch(
            TodoistCreateTaskPayload {
                title,
                description,
                due: TodoistDueDate::Today,
                config: config.todoist.clone(),
                ..Default::default()
            },
            idempotency_key,
            services,
        )
        .await?;

        Ok(())
    }
}

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Watches your Todoist account and files a task when something happens in it —
an item completed, a comment added, a project archived. Todoist posts the event
to this instance as it happens, so there is no polling delay and nothing to
schedule.

## Before you start

Connect your Todoist account under **Connections**. Events only arrive for
accounts connected that way: Todoist sends them to the application you
authorised, so an API token imported from an older configuration file has
nothing to send them through.

There is no address to copy anywhere. Todoist knows where to post, and starts
posting the moment you authorise the application.

## Choosing which events to act on

The filter is matched against the event by dotted path. Every delivery carries:

- `event_name` — such as `item:completed`, `item:added`, `note:added`,
  `project:archived`, `section:updated`
- `user_id` — the Todoist account it happened in
- `initiator.email` — who did it, which differs from `user_id` in a shared
  project
- `triggered_at`
- `event_data` — the object itself, whose fields depend on the event

```
event_name == "item:completed" && "errand" in event_data.labels
```

`event_data` differs from one event to the next, so send yourself one — complete
a task, add a comment — and look at what arrived before writing anything
elaborate.

## Avoid the loop

**A workflow that files a Todoist task in response to a Todoist event can feed
itself.** The task it creates is an `item:added` event, which arrives back here
and creates another. Keep the filter narrow: name the events you want rather
than excluding the ones you do not, and if you do match `item:added`, exclude
the project you file into.

## Writing the title and description

Both are templates. Write `${{ some.path }}` to insert a value from the event:

```
Follow up: ${{ event_data.content }}
```

A path that is not present renders as nothing rather than failing the delivery,
since events differ in which fields they carry. Rendered output is length-capped,
so a template aimed at a large field cannot produce an unbounded task.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// A vector computed independently of this code, so a change to how the
    /// digest is encoded is caught here rather than by Todoist silently having
    /// every delivery rejected.
    const SECRET: &str = "verification-token";
    const BODY: &str = r#"{"event_name":"item:completed","user_id":"2671355"}"#;

    fn signature(secret: &str, body: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }

    #[test]
    fn verify_signature_accepts_a_valid_signature() {
        assert!(TodoistWebhook::verify_signature(SECRET, BODY, &signature(SECRET, BODY)).is_ok());
    }

    #[test]
    fn verify_signature_rejects_a_tampered_body() {
        let signed = signature(SECRET, BODY);
        assert!(
            TodoistWebhook::verify_signature(SECRET, r#"{"event_name":"item:added"}"#, &signed)
                .is_err()
        );
    }

    #[test]
    fn verify_signature_rejects_the_wrong_secret() {
        assert!(TodoistWebhook::verify_signature(SECRET, BODY, &signature("other", BODY)).is_err());
    }

    /// GitHub prefixes its digest with `sha256=`; Todoist does not. Accepting
    /// the prefixed form would mean accepting a signature we never checked.
    #[test]
    fn verify_signature_rejects_a_hex_encoded_signature() {
        let mut mac = HmacSha256::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(BODY.as_bytes());
        let hex = hex::encode(mac.finalize().into_bytes());

        assert!(TodoistWebhook::verify_signature(SECRET, BODY, &hex).is_err());
    }

    async fn signing_key(webhook_secret: Option<&str>) -> Option<String> {
        let webhook_secret = webhook_secret.map(str::to_string);

        let context = crate::services::AppContext::new_mock(move |config| {
            config.connections.todoist.app = Some(crate::config::TodoistAppConfig {
                client_id: "client".into(),
                client_secret: "client-secret".into(),
                _webhook_secret: webhook_secret.clone(),
                scopes: vec!["data:read_write".into()],
                api_url: None,
                acl: None,
            });
        })
        .await
        .unwrap();

        TodoistWebhook.secret(&context.config())
    }

    #[tokio::test]
    async fn the_client_secret_is_used_even_when_a_verification_token_is_configured() {
        assert_eq!(signing_key(None).await.as_deref(), Some("client-secret"));
        assert_eq!(
            signing_key(Some("verification-token")).await.as_deref(),
            Some("client-secret")
        );
    }
}
