use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha512;

use crate::config::TodoistConfig;
use crate::prelude::*;

type HmacSha512 = Hmac<Sha512>;

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct TerraformWebhookConfig {
    #[serde(default)]
    pub secret: Option<String>,

    #[serde(default = "default_todoist_config")]
    pub todoist: TodoistConfig,
}

fn default_todoist_config() -> TodoistConfig {
    TodoistConfig {
        project: Some("Hobbies".into()),
        section: Some("Open Source".into()),
        ..Default::default()
    }
}

pub struct TerraformWebhook;

impl Job for TerraformWebhook {
    type JobType = WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/terraform"
    }

    #[instrument("webhooks.terraform.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        if let Some(secret) = services.config().webhooks.terraform.secret.as_ref() {
            let expected_hash = job.headers.get("X-TFE-Notification-Signature")
                .ok_or_else(|| human_errors::user("Missing X-TFE-Notification-Signature header in Terraform webhook", &[
                    "Make sure you are only sending Terraform Cloud webhook events to this endpoint."
                ]))?;

            let expected_tag = hex::decode(expected_hash).wrap_user_err(
                "Invalid X-TFE-Notification-Signature header format in Terraform webhook",
                &["Make sure the sender of the webhook is sending a valid HMAC SHA-512 signature."],
            )?;

            let mut mac = HmacSha512::new_from_slice(secret.as_bytes())
                .or_user_err(&[
                    "Make sure that you have provided a valid webhooks.terraform.secret in your config file."
                ])?;

            mac.update(job.body.as_bytes());
            mac.verify_slice(expected_tag.as_slice())
                .wrap_user_err("The Terraform webhook's signature did not match the content of the webhook payload.",
                &[
                    "Make sure the sender of the webhook is sending the correct signature using the configured secret."
                ])?;
        }

        let payload: NotificationPayload = job.json()?;

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
                        due: crate::publishers::TodoistDueDate::None,
                        config: services
                            .config()
                            .connections
                            .todoist
                            .merge(&default_todoist_config()),
                        ..Default::default()
                    },
                    None,
                    &services,
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
                        due: crate::publishers::TodoistDueDate::None,
                        config: services
                            .config()
                            .connections
                            .todoist
                            .merge(&default_todoist_config()),
                        ..Default::default()
                    },
                    None,
                    &services,
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
    use super::*;

    #[test]
    fn test_deserialization_v1() {
        let payload_v1 = r#"
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

        let deserialized_v1: NotificationPayload = serde_json::from_str(payload_v1).unwrap();
        assert!(
            matches!(deserialized_v1, NotificationPayload::Standard { run_id, .. } if run_id == "run_123456")
        );
    }

    #[test]
    fn test_deserialization_v2() {
        let payload_v2 = r#"
        {
            "payload_version": 2,
            "notification_configuration_id": "nc_654321",
            "notification_configuration_url": "https://app.terraform.io/app/org/workspaces/ws/notifications/nc_654321",
            "trigger_scope": "assessment",
            "trigger": "assessment:drifted",
            "message": "Drift detected in workspace.",
            "details": {}
        }"#;
        let deserialized_v2: NotificationPayload = serde_json::from_str(payload_v2).unwrap();
        assert!(
            matches!(deserialized_v2, NotificationPayload::Workplace { notification_configuration_id, .. } if notification_configuration_id == "nc_654321")
        );
    }

    #[test]
    fn test_deserialization_sampled() {
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
}
