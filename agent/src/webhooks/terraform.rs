use serde::{Deserialize, Serialize};

use crate::prelude::*;
use crate::publishers::TodoistTarget;
use crate::webhooks::WebhookDelivery;

/// What one person asked us to do with their Terraform Cloud notifications.
///
/// There is deliberately no signing secret here. HMAC signing existed because
/// the endpoint Terraform posted to was the same for everybody, so the signature
/// was the only thing distinguishing a real notification from anybody who knew
/// the URL; a workflow now has its own unguessable URL that its owner can
/// rotate, and that answers the same question without a secret to keep in step
/// across two systems.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct TerraformWebhookConfig {
    /// What to call this workflow, so that somebody watching two organisations
    /// can tell which of them filed a task.
    pub name: String,

    #[serde(default = "default_todoist_config")]
    pub todoist: TodoistTarget,
}

impl std::fmt::Display for TerraformWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "terraform/{}", self.name)
    }
}

fn default_todoist_config() -> TodoistTarget {
    TodoistTarget {
        project: Some("Hobbies".into()),
        section: Some("Open Source".into()),
        ..Default::default()
    }
}

pub struct TerraformWebhook;

crate::register_job!(TerraformWebhook);
crate::register_workflow_type!(TerraformWebhook);

impl crate::workflows::ConfigurableWorkflow for TerraformWebhook {
    type ConfigType = TerraformWebhookConfig;

    fn type_id() -> &'static str {
        "terraform"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Terraform Cloud".to_string(),
            description: "Files a task for each notification a Terraform Cloud workspace sends."
                .to_string(),
            trigger: WorkflowTrigger::Webhook {
                // Must name the same partition this job consumes from, or a
                // delivery would be queued somewhere nothing is reading.
                source: "terraform".to_string(),
            },
            fields: [FieldDescriptor::new(
                crate::config_path!(TerraformWebhookConfig: name),
                "Name",
                FieldKind::Text {
                    placeholder: Some("Infrastructure".into()),
                },
            )
            .with_help("Used to label this workflow, so you can tell it apart from your others.")
            .required()]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                TerraformWebhookConfig,
                project = Some("Hobbies"),
                section = Some("Open Source")
            ))
            .collect(),
        }
    }
}

impl Job for TerraformWebhook {
    type JobType = WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/terraform"
    }

    #[instrument("webhooks.terraform.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the payload, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<TerraformWebhookConfig>(services).await? else {
            return Ok(());
        };

        let payload: NotificationPayload = job.event.json()?;

        match &payload {
            NotificationPayload::Standard {
                organization_name,
                workspace_name,
                run_message,
                run_url,
                notifications,
                ..
            } => {
                crate::publishers::TodoistCreateTask::dispatch(
                    crate::publishers::TodoistCreateTaskPayload {
                        title: format!(
                            "[**terraform:{}/{}**]({}): {}",
                            organization_name, workspace_name, run_url, run_message
                        ),
                        description: Some(
                            notifications
                                .iter()
                                .map(|n| {
                                    format!(
                                        "- \\[{}\\] {} (by {} at {})",
                                        n.trigger,
                                        n.message,
                                        n.run_updated_by.as_deref().unwrap_or("unknown"),
                                        n.run_updated_at
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        priority: Some(payload.priority()),
                        due: crate::publishers::TodoistDueDate::DateTime(ctx.scheduled_at()),
                        config: config.todoist.clone(),
                        ..Default::default()
                    },
                    None,
                    services,
                )
                .await?;
            }
            NotificationPayload::Workplace {
                message, details, ..
            } => {
                crate::publishers::TodoistCreateTask::dispatch(
                    crate::publishers::TodoistCreateTaskPayload {
                        title: format!("**Terraform Cloud**: {}", message),
                        description: Some(format!(
                            "```\n{}\n```",
                            serde_json::to_string_pretty(&details).or_system_err(&[
                                "Please report this issue to the development team on GitHub."
                            ])?
                        )),
                        priority: Some(payload.priority()),
                        due: crate::publishers::TodoistDueDate::DateTime(ctx.scheduled_at()),
                        config: config.todoist.clone(),
                        ..Default::default()
                    },
                    None,
                    services,
                )
                .await?;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct NotificationVersion<const V: u8>;

impl<const V: u8> NotificationVersion<V> {
    pub const ERROR: &'static str = "Invalid notification version";
}

impl<const V: u8> PartialEq<NotificationVersion<V>> for u8 {
    fn eq(&self, _: &NotificationVersion<V>) -> bool {
        V == *self
    }
}

impl<const V: u8> Serialize for NotificationVersion<V> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u8(V)
    }
}

impl<'de, const V: u8> Deserialize<'de> for NotificationVersion<V> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        if value == V {
            Ok(NotificationVersion::<V>)
        } else {
            Err(serde::de::Error::custom(Self::ERROR))
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NotificationPayload {
    Standard {
        payload_version: NotificationVersion<1>,
        notification_configuration_id: String,
        run_url: String,
        run_id: String,
        run_message: String,
        run_created_at: chrono::DateTime<chrono::Utc>,
        run_created_by: String,
        workspace_id: String,
        workspace_name: String,
        organization_name: String,
        notifications: Vec<NotificationV1>,
    },
    Workplace {
        payload_version: NotificationVersion<2>,
        notification_configuration_id: String,
        notification_configuration_url: String,
        trigger_scope: String,
        trigger: String,
        message: String,
        details: serde_json::Value,
    },
}

impl NotificationPayload {
    pub fn priority_for(trigger: &str) -> i32 {
        match trigger {
            "run:errored" => 4,
            "run:needs_attention" => 4,
            "assessment:drifted" => 4,
            "assessment:check_failure" => 4,
            "assessment:failed" => 4,
            "run:completed" => 2,
            _ => 1,
        }
    }

    pub fn priority(&self) -> i32 {
        match self {
            NotificationPayload::Standard { notifications, .. } => notifications
                .iter()
                .map(|n| Self::priority_for(&n.trigger))
                .max()
                .unwrap_or(1),
            NotificationPayload::Workplace { trigger, .. } => Self::priority_for(trigger),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct NotificationV1 {
    pub message: String,
    pub trigger: String,
    pub run_status: String,
    pub run_updated_at: chrono::DateTime<chrono::Utc>,
    pub run_updated_by: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::webhooks::WebhookEvent;
    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    const RUN_NOTIFICATION: &str = r#"
    {
        "payload_version": 1,
        "notification_configuration_id": "nc_123456",
        "run_url": "https://app.terraform.io/app/org/workspaces/ws/runs/run_123456",
        "run_id": "run_123456",
        "run_message": "Apply complete!",
        "run_created_at": "2024-01-01T12:00:00Z",
        "run_created_by": "example_user",
        "workspace_id": "ws_123456",
        "workspace_name": "example_workspace",
        "organization_name": "example_org",
        "notifications": [
            {
                "message": "Run completed successfully.",
                "trigger": "run:completed",
                "run_status": "completed",
                "run_updated_at": "2024-01-01T12:30:00Z",
                "run_updated_by": "example_user"
            }
        ]
    }"#;

    const WORKSPACE_NOTIFICATION: &str = r#"
    {
        "payload_version": 2,
        "notification_configuration_id": "nc_654321",
        "notification_configuration_url": "https://app.terraform.io/app/org/workspaces/ws/notifications/nc_654321",
        "trigger_scope": "assessment",
        "trigger": "assessment:drifted",
        "message": "Drift detected in workspace.",
        "details": {}
    }"#;

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "terraform".into(),
                config,
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id
    }

    fn delivery(workflow: automate_api::WorkflowId, body: impl Into<String>) -> WebhookDelivery {
        WebhookDelivery {
            workflow,
            event: WebhookEvent {
                body: body.into(),
                query: String::new(),
                headers: HashMap::new(),
            },
        }
    }

    async fn run(
        services: &(impl Services + Send + Sync + Clone + 'static),
        delivery: &WebhookDelivery,
    ) -> Result<(), human_errors::Error> {
        TerraformWebhook
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                delivery,
            )
            .await
    }

    async fn filed(
        services: &(impl Services + Send + Sync + 'static),
    ) -> Vec<crate::db::PeekedMessage<serde_json::Value>> {
        services
            .queue()
            .peek("todoist/create-task", 10)
            .await
            .expect("peek the todoist queue")
    }

    #[test]
    fn a_run_notification_is_read_as_terraform_sends_it() {
        let deserialized: NotificationPayload = serde_json::from_str(RUN_NOTIFICATION).unwrap();
        assert!(
            matches!(deserialized, NotificationPayload::Standard { run_id, .. } if run_id == "run_123456")
        );
    }

    #[test]
    fn a_workspace_notification_is_read_as_terraform_sends_it() {
        let deserialized: NotificationPayload =
            serde_json::from_str(WORKSPACE_NOTIFICATION).unwrap();
        assert!(
            matches!(deserialized, NotificationPayload::Workplace { notification_configuration_id, .. } if notification_configuration_id == "nc_654321")
        );
    }

    #[test]
    fn a_notification_from_a_run_nobody_updated_is_still_read() {
        // `run_updated_by` is null for anything a bot started, which is most of
        // them, so a payload sampled from the wire is worth keeping around.
        let payload = r#"{
            "payload_version":1,
            "notification_configuration_id":"nc-a9UxE3zM5k6YSNK3",
            "run_url":"https://app.terraform.io/app/xxx/yyy/runs/run-xboqtF5JxofL6a6A",
            "run_id":"run-xboqtF5JxofL6a6A",
            "run_message":"Merge pull request #100 from xxx/dependabot/terraform/hashicorp/azurerm-4.57.0",
            "run_created_at":"2025-12-18T19:13:53.000Z",
            "run_created_by":"dependabot[bot]",
            "workspace_id":"ws-qsGnTma1RXJ",
            "workspace_name":"infra",
            "organization_name":"xxx",
            "notifications":[
                {
                    "message":"Run Planned and Finished",
                    "trigger":"run:completed",
                    "run_status":"planned_and_finished",
                    "run_updated_at":"2025-12-18T19:15:00.000Z",
                    "run_updated_by":null
                }
            ]
        }"#;
        let deserialized: NotificationPayload = serde_json::from_str(payload).unwrap();
        assert!(
            matches!(deserialized, NotificationPayload::Standard { run_id, .. } if run_id == "run-xboqtF5JxofL6a6A")
        );
    }

    #[tokio::test]
    async fn a_run_notification_files_a_task_naming_the_workspace_it_came_from() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Infrastructure" })).await;

        run(&services, &delivery(workflow, RUN_NOTIFICATION))
            .await
            .unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"],
            "[**terraform:example_org/example_workspace**](https://app.terraform.io/app/org/workspaces/ws/runs/run_123456): Apply complete!",
        );
    }

    #[tokio::test]
    async fn a_workspace_notification_files_a_task_carrying_its_details() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Infrastructure" })).await;

        run(&services, &delivery(workflow, WORKSPACE_NOTIFICATION))
            .await
            .unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"],
            "**Terraform Cloud**: Drift detected in workspace.",
        );
        assert_eq!(
            filed[0].payload["priority"], 4,
            "drift is something somebody has to go and look at",
        );
    }

    #[tokio::test]
    async fn a_delivery_for_a_workflow_that_is_gone_stops_there() {
        // Deliveries queue behind one another, so a workflow can be deleted
        // while one of its own is still waiting to run.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Infrastructure" })).await;

        WorkflowStore::new(&services)
            .with_index(&services)
            .delete(workflow)
            .await
            .unwrap();

        run(&services, &delivery(workflow, RUN_NOTIFICATION))
            .await
            .expect("a deleted workflow should not fail the delivery");

        assert!(filed(&services).await.is_empty());
    }

    #[test]
    fn deliveries_are_queued_where_this_workflow_reads_them() {
        // The trigger decides where a configuration is stored and the job
        // decides where deliveries are queued. A mismatch between the two is a
        // workflow that saves happily and never runs.
        use crate::workflows::ConfigurableWorkflow;

        assert_eq!(
            TerraformWebhook::descriptor().trigger.partition(),
            <TerraformWebhook as Job>::partition(),
        );
    }
}
