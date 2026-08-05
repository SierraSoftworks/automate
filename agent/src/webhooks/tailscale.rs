use std::fmt::Display;

use chrono::DateTime;
use serde::{Deserialize, Serialize};

use crate::{
    prelude::*,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate},
};

/// What one person asked us to do with the events their tailnet reports.
///
/// There is deliberately no shared secret here, and no signature check.
/// Tailscale's HMAC used to be the only thing standing between this endpoint
/// and anybody who knew the installation's hostname, because the address itself
/// was public knowledge — one `/webhooks/tailscale` for the whole installation.
/// A workflow now has its own unguessable address which nothing else can be
/// reached at and which its owner can rotate, so the secret would be a second
/// thing to configure that proves nothing the address has not already proven.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TailscaleWebhookConfig {
    /// What to call this workflow, so it can be told apart from the others in a
    /// list of them.
    pub name: String,

    #[serde(default)]
    pub filter: crate::filter::Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: crate::publishers::TodoistTarget,
}

impl Display for TailscaleWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tailscale/{}", self.name)
    }
}

fn default_todoist_config() -> crate::publishers::TodoistTarget {
    crate::publishers::TodoistTarget {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct TailscaleWebhook;

crate::register_job!(TailscaleWebhook);
crate::register_workflow_type!(TailscaleWebhook);

impl crate::workflows::ConfigurableWorkflow for TailscaleWebhook {
    type ConfigType = TailscaleWebhookConfig;

    fn type_id() -> &'static str {
        "tailscale"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Tailscale".to_string(),
            description: "Files a task when your tailnet reports something that needs a person."
                .to_string(),
            trigger: WorkflowTrigger::Webhook {
                source: "tailscale".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(TailscaleWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Tailnet alerts".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(TailscaleWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec!["type".into(), "tailnet".into(), "message".into()],
                    },
                )
                .with_help(
                    "Only file the events matching this, such as type == \"nodeNeedsApproval\". Leave it empty to file every event this webhook is subscribed to.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                TailscaleWebhookConfig,
                project = Some("Life"),
                section = Some("Tasks & Chores")
            ))
            .collect(),
        }
    }
}

impl Job for TailscaleWebhook {
    type JobType = crate::webhooks::WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/tailscale"
    }

    #[instrument("webhooks.tailscale.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the delivery, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<TailscaleWebhookConfig>(services).await? else {
            return Ok(());
        };

        // Tailscale delivers webhook events as a JSON array, even when only a
        // single event is included. https://tailscale.com/kb/1213/webhooks
        let events: Vec<TailscaleAlertEventPayload> = job.event.json()?;

        for event in events {
            if !config.filter.matches(&event)? {
                info!(
                    "Tailscale event '{}' did not match filter; ignoring.",
                    event._type
                );
                continue;
            }

            let pretty_payload = serde_json::to_string_pretty(&event.data)
                .unwrap_or_else(|_| job.event.body.clone());

            TodoistCreateTask::dispatch(
                TodoistCreateTaskPayload {
                    title: format!(
                        "[**Tailscale**](https://login.tailscale.com/admin): {}",
                        event.message
                    ),
                    description: Some(format!("```\n{pretty_payload}\n```")),
                    due: TodoistDueDate::DateTime(event.timestamp),
                    priority: Some(match event._type.as_str() {
                        "exitNodeIPForwardingNotEnabled" => 4,
                        "subnetIPForwardingNotEnabled" => 4,
                        "nodeNeedsApproval" => 4,
                        "nodeKeyExpired" => 4,
                        "userNeedsApproval" => 4,

                        "policyUpdate" => 3,
                        "nodeCreated" => 3,
                        "nodeApproved" => 3,
                        "nodeKeyExpiringInOneDay" => 3,
                        "userCreated" => 3,
                        "userApproved" => 3,
                        "userRoleUpdated" => 3,

                        "nodeDeleted" => 2,
                        "webhookUpdated" => 2,
                        "webhookDeleted" => 2,

                        "test" => 1,

                        _ => 3,
                    }),
                    config: config.todoist.clone(),
                    ..Default::default()
                },
                None,
                services,
            )
            .await?;
        }

        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct TailscaleAlertEventPayload {
    version: u32,
    timestamp: DateTime<chrono::Utc>,
    #[serde(rename = "type")]
    _type: String,
    tailnet: String,
    message: String,
    data: serde_json::Value,
}

impl Filterable for TailscaleAlertEventPayload {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        match key {
            "type" => self._type.as_str().into(),
            "tailnet" => self.tailnet.as_str().into(),
            "message" => self.message.as_str().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;

    use crate::{
        webhooks::{WebhookDelivery, WebhookEvent},
        workflow_store::{WorkflowDraft, WorkflowStore},
    };

    use super::*;

    /// A delivery as Tailscale sends it: an array, even for a single event.
    const POLICY_UPDATE: &str = r#"[{"timestamp":"2026-06-19T21:12:52.923385657Z","version":1,"type":"policyUpdate","tailnet":"example.org.github","message":"Tailnet policy file updated","data":{"url":"https://login.tailscale.com/admin/acls"}}]"#;

    /// A configuration that files every event the tailnet reports.
    fn config() -> serde_json::Value {
        serde_json::json!({ "name": "Tailnet alerts" })
    }

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "tailscale".into(),
                config,
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id
    }

    fn delivery(workflow: automate_api::WorkflowId, body: &str) -> WebhookDelivery {
        WebhookDelivery {
            workflow,
            event: WebhookEvent {
                body: body.to_string(),
                query: String::new(),
                headers: HashMap::new(),
            },
        }
    }

    /// Runs one delivery the way the consumer would.
    async fn run(
        services: &(impl Services + Send + Sync + Clone + 'static),
        delivery: &WebhookDelivery,
    ) -> Result<(), human_errors::Error> {
        TailscaleWebhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                delivery,
            )
            .await
    }

    /// The tasks this workflow asked to have created.
    async fn filed(
        services: &(impl Services + Send + Sync + 'static),
    ) -> Vec<crate::db::PeekedMessage<serde_json::Value>> {
        services
            .queue()
            .peek("todoist/create-task", 10)
            .await
            .expect("peek the todoist queue")
    }

    #[tokio::test]
    async fn an_event_files_a_task_carrying_tailscales_own_wording() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, POLICY_UPDATE))
            .await
            .unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"],
            "[**Tailscale**](https://login.tailscale.com/admin): Tailnet policy file updated",
        );
        assert!(
            filed[0].payload["description"]
                .as_str()
                .unwrap()
                .contains("https://login.tailscale.com/admin/acls"),
            "the event's own payload should be quoted into the task, so it can be triaged without going to look",
        );
    }

    #[tokio::test]
    async fn a_node_waiting_for_approval_outranks_a_policy_change() {
        // Somebody is blocked until a node is approved, whereas a policy update
        // is a note to self, and a task list that cannot tell them apart is a
        // task list nobody reads top-down.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let needs_approval = POLICY_UPDATE.replace("policyUpdate", "nodeNeedsApproval");
        run(&services, &delivery(workflow, &needs_approval))
            .await
            .unwrap();

        assert_eq!(filed(&services).await[0].payload["priority"], 4);
    }

    #[tokio::test]
    async fn an_event_the_filter_rejects_files_nothing() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Approvals only",
                "filter": r#"type == "nodeNeedsApproval""#,
            }),
        )
        .await;

        run(&services, &delivery(workflow, POLICY_UPDATE))
            .await
            .unwrap();

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn every_event_in_one_delivery_gets_its_own_task() {
        // Tailscale batches, so a delivery that only produced one task would
        // quietly drop everything after the first.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let batch = format!(
            "[{},{}]",
            &POLICY_UPDATE[1..POLICY_UPDATE.len() - 1],
            &POLICY_UPDATE[1..POLICY_UPDATE.len() - 1].replace("policyUpdate", "nodeCreated"),
        );

        run(&services, &delivery(workflow, &batch)).await.unwrap();

        assert_eq!(filed(&services).await.len(), 2);
    }

    #[tokio::test]
    async fn a_delivery_for_a_paused_workflow_files_nothing() {
        // Pausing exists so somebody can silence a chatty tailnet without losing
        // their configuration, which only works if a paused workflow is quiet.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        WorkflowStore::new(&services)
            .with_index(&services)
            .update(
                workflow,
                WorkflowDraft {
                    type_id: "tailscale".into(),
                    config: config(),
                    schedule: None,
                    enabled: false,
                },
            )
            .await
            .unwrap();

        run(&services, &delivery(workflow, POLICY_UPDATE))
            .await
            .unwrap();

        assert!(filed(&services).await.is_empty());
    }

    #[test]
    fn a_stored_configuration_names_the_workflow_it_describes() {
        let workflow = crate::workflows::lookup("tailscale").expect("the type is registered");

        assert_eq!(
            workflow
                .describe(&serde_json::json!({ "name": "Tailnet alerts" }))
                .unwrap(),
            "Tailnet alerts",
        );
    }

    #[test]
    fn test_parse_event_array() {
        // Tailscale delivers events as a JSON array, even for a single event.
        let events: Vec<TailscaleAlertEventPayload> =
            serde_json::from_str(POLICY_UPDATE).expect("array payload should parse");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]._type, "policyUpdate");
        assert_eq!(events[0].tailnet, "example.org.github");
        assert_eq!(events[0].message, "Tailnet policy file updated");
    }
}
