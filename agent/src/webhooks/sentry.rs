use std::fmt::Display;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    prelude::*,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate},
};

/// What one person asked us to do with the deliveries Sentry sends them.
///
/// There is deliberately no shared secret here, and no signature check.
/// Sentry's HMAC used to be the only thing standing between this endpoint and
/// anybody who knew the installation's hostname, because the address itself was
/// public knowledge — one `/webhooks/sentry` for the whole installation. A
/// workflow now has its own unguessable address which nothing else can be
/// reached at and which its owner can rotate, so the secret would be a second
/// thing to configure that proves nothing the address has not already proven.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SentryWebhookConfig {
    /// What to call this workflow, so it can be told apart from the others in a
    /// list of them.
    pub name: String,

    #[serde(default)]
    pub filter: crate::filter::Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: crate::publishers::TodoistTarget,
}

impl Display for SentryWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sentry/{}", self.name)
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
pub struct SentryAlertsWebhook;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Files a Todoist task when Sentry reports a newly created issue or fires an
issue alert. The task's title carries the issue's short id and links to it in
Sentry; its body is the culprit — the function or file Sentry blamed — so a
glance is usually enough to decide whether it needs you now.

Priority follows the issue's level: `fatal` and `error` are raised, `warning`
sits below them, and `info` and `debug` lowest.

## Two payload shapes

Sentry has two entirely different ways of posting to a webhook, and this
workflow accepts both:

- An **internal integration** posts *issue* events, and only those whose action
  is `created` are filed. Sentry re-notifies as an issue is assigned, resolved
  and ignored, and a task per update would bury the one that said the error was
  new.
- An **issue alert rule** with a webhook action posts a different document
  entirely, describing the alert rather than the issue. Those are filed as they
  arrive, since the alert rule has already done the selecting.

Which one you set up decides what the filter can see, so it is worth knowing
which you configured.

## Getting the address

Save the workflow first. Its address is generated when it is created and shown
on the workflow afterwards; there is nothing to paste into Sentry until then.

Then, in Sentry, either:

- **For issue events**: open your organisation's settings, go to *Developer
  Settings* → **Custom Integrations**, create an internal integration, set its
  webhook URL to this workflow's address, and subscribe it to **issue**
  events; or
- **For alerts**: open **Alerts**, edit or create an issue alert rule, and add
  a *Send a notification via a webhook* action pointing at this workflow's
  address.

Sentry signs its deliveries, but nothing is checked here: the address is
unguessable and can be rotated, which is what a signature would have been
proving, and one thing to keep in step is better than two.

## Choosing which issues to file

The filter runs against each delivery. Both shapes answer to `issue_id`,
`issue_title`, `issue_level` and `project_name`. Only the integration payload
carries `action`, `issue_type` and `project_platform`, so a filter naming those
will silently reject every alert-rule delivery.

```
issue_level in ["fatal", "error"]
```

Leave it empty to file everything Sentry sends here.
"#;

crate::register_job!(SentryAlertsWebhook);
crate::register_workflow_type!(SentryAlertsWebhook);

impl crate::workflows::ConfigurableWorkflow for SentryAlertsWebhook {
    type ConfigType = SentryWebhookConfig;

    fn type_id() -> &'static str {
        "sentry"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Sentry".to_string(),
            description: "Files a task when Sentry reports a new issue or fires an alert."
                .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                source: "sentry".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(SentryWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Production errors".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(SentryWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        // The names both delivery shapes answer to; an issue
                        // alert has no `action` or platform of its own, so a
                        // filter naming those only ever matches the integration
                        // payload.
                        fields: vec![
                            "action".into(),
                            "issue_id".into(),
                            "issue_title".into(),
                            "issue_type".into(),
                            "issue_level".into(),
                            "project_name".into(),
                            "project_platform".into(),
                        ],
                    },
                )
                .with_help(
                    "Only file the issues matching this, such as issue_level in [\"fatal\", \"error\"]. Leave it empty to file every issue Sentry sends.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                SentryWebhookConfig,
                project = Some("Life"),
                section = Some("Tasks & Chores")
            ))
            .collect(),
        }
    }
}

impl Job for SentryAlertsWebhook {
    type JobType = crate::webhooks::WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/sentry"
    }

    #[instrument("webhooks.sentry.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the delivery, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<SentryWebhookConfig>(services).await? else {
            return Ok(());
        };

        let notification: SentryNotification = job.event.json()?;

        match notification {
            SentryNotification::Integration(integration) => {
                // Only process created issues (new errors)
                if !integration.action.eq_ignore_ascii_case("created") {
                    info!("Ignoring non-created Sentry issue: {}", integration.action);
                    return Ok(());
                }

                if !config.filter.matches(&integration)? {
                    info!(
                        "Sentry issue '{}' did not match filter; ignoring.",
                        integration.data.issue.title
                    );
                    return Ok(());
                }

                let issue = &integration.data.issue;

                TodoistCreateTask::dispatch(
                    TodoistCreateTaskPayload {
                        title: format!("[{}]({}): {}", issue.short_id, issue.web_url, issue.title),
                        description: Some(issue.culprit.clone()),
                        due: TodoistDueDate::DateTime(ctx.scheduled_at()),
                        priority: Some(issue.level.to_priority()),
                        config: config.todoist.clone(),
                        ..Default::default()
                    },
                    None,
                    services,
                )
                .await?;
            }
            SentryNotification::Alert(alert) => {
                if !config.filter.matches(&alert)? {
                    info!(
                        "Sentry alert '{}' did not match filter; ignoring.",
                        alert.title()
                    );
                    return Ok(());
                }

                TodoistCreateTask::dispatch(
                    TodoistCreateTaskPayload {
                        title: format!(
                            "[{}]({}): {}",
                            alert.project_slug,
                            alert.url,
                            alert.title()
                        ),
                        description: Some(alert.culprit.clone()),
                        due: TodoistDueDate::DateTime(ctx.scheduled_at()),
                        priority: Some(alert.level.to_priority()),
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

/// Represents the two different Sentry webhook payload formats:
/// - Integration Platform webhooks (with `action`, `actor`, `data`)
/// - Issue Alert webhooks (with `id`, `project`, `level`, `url`, `event`)
#[derive(Deserialize)]
#[serde(untagged)]
enum SentryNotification {
    Integration(SentryIssueNotification),
    Alert(SentryAlertNotification),
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct SentryIssueNotification {
    action: String,
    actor: SentryActor,
    data: SentryIssueData,
}

impl Filterable for SentryIssueNotification {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        match key {
            "action" => self.action.as_str().into(),
            "issue_id" => self.data.issue.id.as_str().into(),
            "issue_title" => self.data.issue.title.as_str().into(),
            "issue_type" => format!("{}", self.data.issue._type).into(),
            "issue_level" => format!("{}", self.data.issue.level).into(),
            "project_name" => self.data.issue.project.name.as_str().into(),
            "project_platform" => self.data.issue.project.platform.as_str().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct SentryAlertNotification {
    id: String,
    project_slug: String,
    level: SentryIssueLevel,
    culprit: String,
    url: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    triggering_rules: Vec<String>,
    #[serde(default)]
    event: Value,
}

impl SentryAlertNotification {
    fn title(&self) -> String {
        // Try to get a meaningful title from the event metadata
        if let Some(metadata) = self.event.get("metadata") {
            let error_type = metadata.get("type").and_then(|v| v.as_str());
            let error_value = metadata.get("value").and_then(|v| v.as_str());

            match (error_type, error_value) {
                (Some(t), Some(v)) if !t.is_empty() && !v.is_empty() => {
                    return format!("{}: {}", t, v);
                }
                (Some(t), _) if !t.is_empty() => return t.to_string(),
                (_, Some(v)) if !v.is_empty() => return v.to_string(),
                _ => {}
            }
        }

        // Fall back to the event title if available
        if let Some(title) = self.event.get("title").and_then(|v| v.as_str())
            && !title.is_empty()
        {
            return title.to_string();
        }

        // Final fallback to the message field
        if !self.message.is_empty() {
            return self.message.clone();
        }

        format!("Sentry Alert #{}", self.id)
    }
}

impl Filterable for SentryAlertNotification {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        match key {
            "issue_id" => self.id.as_str().into(),
            "issue_title" => self.title().into(),
            "issue_level" => format!("{}", self.level).into(),
            "project_name" => self.project_slug.as_str().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct SentryActor {
    #[serde(rename = "type")]
    _type: SentryActorType,
    id: String,
    name: String,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum SentryActorType {
    Application,
    User,
}

impl Display for SentryActorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SentryActorType::Application => write!(f, "application"),
            SentryActorType::User => write!(f, "user"),
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct SentryIssueData {
    issue: SentryIssue,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct SentryIssue {
    id: String,
    url: String,
    web_url: String,
    project_url: String,
    title: String,
    #[serde(rename = "type")]
    _type: SentryIssueLevel,
    level: SentryIssueLevel,
    #[serde(rename = "shortId")]
    short_id: String,
    culprit: String,
    project: SentryProject,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum SentryIssueLevel {
    Fatal,
    Error,
    Warning,
    Info,
    Debug,
}

impl SentryIssueLevel {
    pub fn to_priority(&self) -> i32 {
        match self {
            SentryIssueLevel::Fatal | SentryIssueLevel::Error => 3,
            SentryIssueLevel::Warning => 2,
            _ => 1,
        }
    }
}

impl Display for SentryIssueLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SentryIssueLevel::Fatal => write!(f, "fatal"),
            SentryIssueLevel::Error => write!(f, "error"),
            SentryIssueLevel::Warning => write!(f, "warning"),
            SentryIssueLevel::Info => write!(f, "info"),
            SentryIssueLevel::Debug => write!(f, "debug"),
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct SentryProject {
    id: String,
    name: String,
    platform: String,
    slug: String,
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

    /// An Integration Platform delivery: a newly created issue.
    const ISSUE: &str = r#"{"action":"created","actor":{"type":"application","id":"sentry","name":"Sentry"},"data":{"issue":{"id":"123","url":"https://sentry.io/api/0/issues/123/","web_url":"https://sentry.io/issues/123/","project_url":"https://sentry.io/projects/my-project/","title":"Test Error","type":"error","level":"error","shortId":"TEST-1","culprit":"test.js","project":{"id":"1","name":"Test Project","platform":"javascript","slug":"test-project"}}}}"#;

    /// An issue alert delivery, which is a different shape entirely.
    const ALERT: &str = r#"{"id":"7470688687","project":"git-tool","project_name":"git-tool","project_slug":"git-tool","logger":"root","level":"error","culprit":"main in main","message":"","url":"https://sierra-softworks.sentry.io/issues/7470688687/","triggering_rules":["Send a notification"],"event":{"title":"Test Error: something went wrong","metadata":{"type":"Test Error","value":"something went wrong"}}}"#;

    /// A configuration that files every issue Sentry sends.
    fn config() -> serde_json::Value {
        serde_json::json!({ "name": "Production errors" })
    }

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "sentry".into(),
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
        SentryAlertsWebhook
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
    async fn a_newly_created_issue_files_a_task_linking_back_to_sentry() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, ISSUE)).await.unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"],
            "[TEST-1](https://sentry.io/issues/123/): Test Error",
        );
        assert_eq!(
            filed[0].payload["priority"], 3,
            "an error-level issue is worth interrupting somebody for",
        );
    }

    #[tokio::test]
    async fn an_issue_alert_files_a_task_even_though_its_payload_is_a_different_shape() {
        // Sentry sends two entirely different documents depending on how the
        // webhook was set up, and somebody who configured the other one should
        // not be left wondering why nothing happens.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, ALERT)).await.unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert!(
            filed[0].payload["title"]
                .as_str()
                .unwrap()
                .contains("Test Error: something went wrong"),
        );
    }

    #[tokio::test]
    async fn an_issue_that_was_only_updated_files_nothing() {
        // Sentry re-notifies as an issue changes, and a task per update would
        // bury the one that said the error was new.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let resolved = ISSUE.replace(r#""action":"created""#, r#""action":"resolved""#);
        run(&services, &delivery(workflow, &resolved))
            .await
            .unwrap();

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn an_issue_the_filter_rejects_files_nothing() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Fatal only",
                "filter": r#"issue_level == "fatal""#,
            }),
        )
        .await;

        run(&services, &delivery(workflow, ISSUE)).await.unwrap();

        assert!(
            filed(&services).await.is_empty(),
            "an error-level issue should not pass a fatal-only filter",
        );
    }

    #[tokio::test]
    async fn a_delivery_for_a_paused_workflow_files_nothing() {
        // Pausing exists so somebody can silence a noisy project without losing
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
                    type_id: "sentry".into(),
                    config: config(),
                    schedule: None,
                    enabled: false,
                },
            )
            .await
            .unwrap();

        run(&services, &delivery(workflow, ISSUE)).await.unwrap();

        assert!(filed(&services).await.is_empty());
    }

    #[test]
    fn a_stored_configuration_names_the_workflow_it_describes() {
        let workflow = crate::workflows::lookup("sentry").expect("the type is registered");

        assert_eq!(
            workflow
                .describe(&serde_json::json!({ "name": "Production errors" }))
                .unwrap(),
            "Production errors",
        );
    }

    #[test]
    fn test_parse_integration_webhook() {
        let notification: SentryNotification = serde_json::from_str(ISSUE).unwrap();
        assert!(
            matches!(notification, SentryNotification::Integration(_)),
            "Should parse as Integration webhook"
        );
    }

    #[test]
    fn test_parse_alert_webhook() {
        let notification: SentryNotification = serde_json::from_str(ALERT).unwrap();
        assert!(
            matches!(notification, SentryNotification::Alert(_)),
            "Should parse as Alert webhook"
        );

        if let SentryNotification::Alert(alert) = notification {
            assert_eq!(alert.id, "7470688687");
            assert_eq!(alert.project_slug, "git-tool");
            assert_eq!(alert.level, SentryIssueLevel::Error);
            assert_eq!(alert.culprit, "main in main");
            assert_eq!(alert.title(), "Test Error: something went wrong");
        }
    }

    #[test]
    fn test_alert_title_from_metadata() {
        let body = r#"{"id":"1","project":"test","project_name":"test","project_slug":"test","logger":"root","level":"error","culprit":"test","message":"","url":"https://sentry.io/issues/1/","event":{"metadata":{"type":"ValueError","value":"invalid literal"}}}"#;
        let notification: SentryNotification = serde_json::from_str(body).unwrap();
        if let SentryNotification::Alert(alert) = notification {
            assert_eq!(alert.title(), "ValueError: invalid literal");
        } else {
            panic!("Expected Alert variant");
        }
    }

    #[test]
    fn test_alert_title_fallback_to_event_title() {
        let body = r#"{"id":"1","project":"test","project_name":"test","project_slug":"test","logger":"root","level":"warning","culprit":"test","message":"","url":"https://sentry.io/issues/1/","event":{"title":"Something broke"}}"#;
        let notification: SentryNotification = serde_json::from_str(body).unwrap();
        if let SentryNotification::Alert(alert) = notification {
            assert_eq!(alert.title(), "Something broke");
        } else {
            panic!("Expected Alert variant");
        }
    }

    #[test]
    fn test_alert_title_fallback_to_id() {
        let body = r#"{"id":"42","project":"test","project_name":"test","project_slug":"test","logger":"root","level":"info","culprit":"test","message":"","url":"https://sentry.io/issues/42/","event":{}}"#;
        let notification: SentryNotification = serde_json::from_str(body).unwrap();
        if let SentryNotification::Alert(alert) = notification {
            assert_eq!(alert.title(), "Sentry Alert #42");
        } else {
            panic!("Expected Alert variant");
        }
    }

    #[test]
    fn test_parse_real_alert_payload() {
        // Minimal reproduction of the real production payload structure
        let body = r#"{"id":"7470688687","project":"git-tool","project_name":"git-tool","project_slug":"git-tool","logger":"root","level":"error","culprit":"main in main","message":"","url":"https://sierra-softworks.sentry.io/issues/7470688687/?referrer=webhooks_plugin","triggering_rules":["Send a notification for high priority issues"],"event":{"event_id":"738801da5c4741dc8f201c2ac4197b6e","level":"error","type":"error","title":"The following languages are not supported: The following languages are not supported: nodejs","metadata":{"type":"The following languages are not supported","value":"The following languages are not supported: nodejs"},"platform":"go","timestamp":1778377466.0}}"#;
        let notification: SentryNotification = serde_json::from_str(body).unwrap();
        if let SentryNotification::Alert(alert) = notification {
            assert_eq!(alert.id, "7470688687");
            assert_eq!(alert.project_slug, "git-tool");
            assert_eq!(alert.level, SentryIssueLevel::Error);
            assert_eq!(
                alert.title(),
                "The following languages are not supported: The following languages are not supported: nodejs"
            );
            assert_eq!(alert.level.to_priority(), 3);
        } else {
            panic!("Expected Alert variant");
        }
    }
}
