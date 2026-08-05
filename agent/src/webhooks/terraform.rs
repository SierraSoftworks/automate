use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha512;

use crate::prelude::*;
use crate::publishers::TodoistTarget;
use crate::webhooks::WebhookDelivery;

type HmacSha512 = Hmac<Sha512>;

/// What one person asked us to do with their Terraform Cloud notifications.
///
/// This keeps its signing token. The per-workflow address answers "did somebody
/// who knew the URL post this", which is not the question the signature answers:
/// Terraform's `X-TFE-Notification-Signature` is an HMAC over the body, so it
/// proves that *Terraform* sent this and that nothing rewrote the payload on the
/// way. A URL cannot make either claim, and a delivery that lies about which
/// workspace drifted or which run failed is worth as little as no delivery at
/// all. This is the same reasoning that kept `X-Hub-Signature-256` on the GitHub
/// webhook.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct TerraformWebhookConfig {
    /// What to call this workflow, so that somebody watching two organisations
    /// can tell which of them filed a task.
    pub name: String,

    /// The HMAC token configured on the Terraform Cloud notification, used to
    /// verify the `X-TFE-Notification-Signature` header. Deliveries are refused
    /// while this is unset — see [`TerraformWebhook::handle`] for why.
    #[serde(default)]
    pub secret: String,

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

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Files a Todoist task for each notification a Terraform Cloud workspace sends —
a run that errored, a run waiting for confirmation, a drift assessment that
found something. The task links to the run and quotes the notification's own
messages, so you can tell whether it needs you before opening Terraform.

Priority follows the trigger: errored runs, runs needing attention and failed
or drifted assessments are raised urgently, completed runs are filed quietly,
and everything else lower still.

## Getting the address

Save the workflow first. Its address is generated when it is created and shown
on the workflow afterwards; there is nothing to paste into Terraform until
then.

Then, in Terraform Cloud, open the workspace, go to **Settings →
Notifications**, and add a notification configuration:

1. Choose **Webhook** as the destination.
2. Set the **Webhook URL** to this workflow's address.
3. Fill in **Token** — see below.
4. Select the triggers you want. Runs and assessments are both supported.

HashiCorp's
[notification documentation](https://developer.hashicorp.com/terraform/cloud-docs/workspaces/settings/notifications)
covers the same ground from their side, including the verification request they
send when the configuration is saved.

## The HMAC token

Terraform signs each notification with an HMAC over the body and sends the
digest in `X-TFE-Notification-Signature`. Checking it proves that Terraform
sent the delivery and that nothing rewrote the payload on the way, which the
address on its own cannot: a delivery that lies about which workspace drifted
or which run failed is worth as little as no delivery at all.

Generate a long random string, paste the same value into Terraform's **Token**
field and into **HMAC token** here. They have to match exactly, since Terraform
computes the signature with its copy and we check it with ours.

**A workflow with no token refuses every delivery.** An empty token is not
treated as "skip the check" — that would leave a workflow silently
unauthenticated at exactly the moment somebody forgot to finish setting it up,
so a missing token fails closed instead. It also means the check cannot be
turned off by clearing the field.

## Where the tasks go

There is no filter on this type: every notification Terraform is configured to
send becomes a task, so choose the triggers on the Terraform side rather than
here. The Todoist fields decide which account, project and section they land
in.
"#;

impl TerraformWebhook {
    /// Verifies the `X-TFE-Notification-Signature` header, which Terraform Cloud
    /// populates with the hex-encoded HMAC-SHA512 of the raw request body, keyed
    /// with the token set on the notification configuration.
    ///
    /// Unlike GitHub's, the header carries no algorithm prefix — it is the
    /// digest on its own.
    ///
    /// See https://developer.hashicorp.com/terraform/cloud-docs/workspaces/settings/notifications
    fn verify_signature(
        secret: &str,
        body: &str,
        signature_header: &str,
    ) -> Result<(), human_errors::Error> {
        let expected_signature = hex::decode(signature_header.trim()).or_user_err(&[
            "The signature in the X-TFE-Notification-Signature header is not valid hex.",
            "Ensure that you are only sending Terraform Cloud notifications to this endpoint.",
        ])?;

        let mut mac = HmacSha512::new_from_slice(secret.as_bytes()).wrap_user_err(
            "Failed to create HMAC instance with the provided token.",
            &["Ensure that you have set a valid HMAC token on this workflow."],
        )?;

        mac.update(body.as_bytes());

        // `verify_slice` compares in constant time, so a wrong signature cannot
        // be walked one byte at a time by timing the rejections.
        mac.verify_slice(&expected_signature).wrap_user_err(
            "Webhook signature verification failed (signatures did not match).".to_string(),
            &[
                "Ensure that the HMAC token on this workflow matches the one set on the notification configuration in Terraform Cloud.",
            ],
        )?;

        Ok(())
    }

    /// HTTP header names are case-insensitive, and what reaches us depends on
    /// whatever proxy handled the request, so the lookup cannot assume a casing.
    fn header<'a>(event: &'a WebhookEvent, name: &str) -> Option<&'a str> {
        event
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

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
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                // Must name the same partition this job consumes from, or a
                // delivery would be queued somewhere nothing is reading.
                source: "terraform".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(TerraformWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Infrastructure".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(TerraformWebhookConfig: secret),
                    "HMAC token",
                    FieldKind::Text {
                        placeholder: Some("a long random string".into()),
                    },
                )
                .with_help(
                    "The HMAC token you set on the notification configuration in Terraform Cloud. It signs the body of each notification, which is what proves Terraform sent it and that nothing altered it on the way. Notifications are ignored while this is empty.",
                ),
            ]
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

        let event = &job.event;

        // Everything below this point happens *before* the payload is parsed, so
        // that a delivery we cannot attribute to Terraform is never interpreted,
        // let alone acted on.
        //
        // A rejection returns `Ok(())` rather than an error: nothing about a bad
        // signature improves by trying again, so raising here would only leave
        // the delivery retrying forever and hiding real failures behind it. The
        // log line is the record that it happened.

        // No token configured means we refuse, rather than accept anything. The
        // alternative — treating an empty token as "skip the check" — would make
        // a workflow silently unauthenticated exactly when somebody forgot to
        // finish setting it up, and a forgotten field should fail closed. It
        // also means the token cannot be neutralised by clearing it, and it is
        // what the GitHub webhook does with its own secret.
        if config.secret.is_empty() {
            warn!(
                "Received a Terraform Cloud notification for a workflow with no HMAC token configured; rejecting request."
            );
            return Ok(());
        }

        let Some(signature) = Self::header(event, "x-tfe-notification-signature") else {
            warn!(
                "Received a Terraform Cloud notification without an X-TFE-Notification-Signature header; rejecting request."
            );
            return Ok(());
        };

        if let Err(err) = Self::verify_signature(&config.secret, &event.body, signature) {
            warn!(
                "Failed to verify Terraform Cloud notification signature, rejecting request: {}",
                err
            );
            return Ok(());
        }

        let payload: NotificationPayload = event.json()?;

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

    /// The HMAC token these tests pretend was set on both this workflow and the
    /// Terraform Cloud notification configuration.
    const TOKEN: &str = "a-long-random-string";

    /// Signs a body the way Terraform Cloud does: bare hex, no algorithm prefix.
    fn sign(secret: &str, body: &str) -> String {
        let mut mac = HmacSha512::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

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

    /// A workflow configured with `token`, which the tests pass as `""` when
    /// they want the no-token-configured case.
    async fn store_with_token(
        services: &(impl Services + Send + Sync + 'static),
        token: &str,
    ) -> automate_api::WorkflowId {
        store(
            services,
            serde_json::json!({ "name": "Infrastructure", "secret": token }),
        )
        .await
    }

    fn delivery_with(
        workflow: automate_api::WorkflowId,
        body: impl Into<String>,
        headers: &[(&str, &str)],
    ) -> WebhookDelivery {
        WebhookDelivery {
            workflow,
            event: WebhookEvent {
                body: body.into(),
                query: String::new(),
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<HashMap<_, _>>(),
            },
        }
    }

    /// A delivery carrying the signature Terraform Cloud would have sent for it.
    fn delivery(workflow: automate_api::WorkflowId, body: &str) -> WebhookDelivery {
        delivery_with(
            workflow,
            body,
            &[("X-TFE-Notification-Signature", &sign(TOKEN, body))],
        )
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

    #[test]
    fn a_notification_signed_with_the_configured_token_is_accepted() {
        TerraformWebhook::verify_signature(TOKEN, RUN_NOTIFICATION, &sign(TOKEN, RUN_NOTIFICATION))
            .expect("a signature Terraform itself would have produced should verify");
    }

    #[test]
    fn a_notification_signed_with_a_different_token_is_refused() {
        // Knowing the URL is not knowing the token, which is the whole point of
        // checking one.
        let signature = sign("somebody-elses-token", RUN_NOTIFICATION);
        assert!(TerraformWebhook::verify_signature(TOKEN, RUN_NOTIFICATION, &signature).is_err());
    }

    #[test]
    fn a_body_altered_after_signing_no_longer_matches_its_signature() {
        // The signature covers the body, so replaying a genuine signature over a
        // payload somebody rewrote in transit has to fail.
        let signature = sign(TOKEN, RUN_NOTIFICATION);
        let tampered = RUN_NOTIFICATION.replace("Apply complete!", "Everything is fine");
        assert!(TerraformWebhook::verify_signature(TOKEN, &tampered, &signature).is_err());
    }

    #[test]
    fn a_signature_that_is_not_hex_at_all_is_refused() {
        // Terraform sends hex; anything else is not a signature we failed to
        // match, it is a header we cannot even read.
        assert!(
            TerraformWebhook::verify_signature(TOKEN, RUN_NOTIFICATION, "not-a-signature").is_err()
        );
    }

    #[tokio::test]
    async fn a_run_notification_files_a_task_naming_the_workspace_it_came_from() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store_with_token(&services, TOKEN).await;

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
        let workflow = store_with_token(&services, TOKEN).await;

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
    async fn a_notification_signed_with_the_wrong_token_files_nothing() {
        // Each workflow carries its own token, so a notification signed for
        // somebody else's must not be acted on by this one.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store_with_token(&services, TOKEN).await;

        let job = delivery_with(
            workflow,
            RUN_NOTIFICATION,
            &[(
                "X-TFE-Notification-Signature",
                &sign("somebody-elses-token", RUN_NOTIFICATION),
            )],
        );

        run(&services, &job)
            .await
            .expect("a mis-signed notification should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_notification_whose_body_was_altered_after_signing_files_nothing() {
        // The signature is what makes the payload trustworthy, so a body that
        // was rewritten between Terraform signing it and us receiving it must
        // never reach the parser.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store_with_token(&services, TOKEN).await;

        let job = delivery_with(
            workflow,
            RUN_NOTIFICATION.replace("example_workspace", "someone_elses_workspace"),
            &[(
                "X-TFE-Notification-Signature",
                &sign(TOKEN, RUN_NOTIFICATION),
            )],
        );

        run(&services, &job)
            .await
            .expect("a tampered notification should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_notification_with_no_signature_header_at_all_files_nothing() {
        // Anybody can post to a URL. Without the header there is nothing to
        // check, and "nothing to check" is not the same as "checks out".
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store_with_token(&services, TOKEN).await;

        let job = delivery_with(workflow, RUN_NOTIFICATION, &[]);

        run(&services, &job)
            .await
            .expect("an unsigned notification should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn the_signature_header_is_recognised_whatever_case_it_arrives_in() {
        // HTTP header names are case-insensitive and whatever proxy sits in
        // front of us is free to renormalise them, so a lowercase header must
        // not read as a missing one.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store_with_token(&services, TOKEN).await;

        let job = delivery_with(
            workflow,
            RUN_NOTIFICATION,
            &[(
                "x-tfe-notification-signature",
                &sign(TOKEN, RUN_NOTIFICATION),
            )],
        );

        run(&services, &job)
            .await
            .expect("a correctly signed notification should be processed");

        assert_eq!(filed(&services).await.len(), 1);
    }

    #[tokio::test]
    async fn notifications_are_refused_while_the_workflow_has_no_token_configured() {
        // The token is optional on the form, so it can genuinely be empty. That
        // is treated as "cannot be verified" rather than "need not be verified":
        // a half-finished workflow should file nothing rather than file whatever
        // anybody who found the URL cares to post.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store_with_token(&services, "").await;

        let job = delivery_with(
            workflow,
            RUN_NOTIFICATION,
            &[(
                "X-TFE-Notification-Signature",
                &sign(TOKEN, RUN_NOTIFICATION),
            )],
        );

        run(&services, &job)
            .await
            .expect("an unverifiable notification should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_delivery_for_a_workflow_that_is_gone_stops_there() {
        // Deliveries queue behind one another, so a workflow can be deleted
        // while one of its own is still waiting to run.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store_with_token(&services, TOKEN).await;

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
