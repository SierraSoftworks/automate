use std::fmt::Display;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    filter::FilterValue,
    prelude::*,
    publishers::TodoistTarget,
    publishers::{
        TodoistCompleteTask, TodoistCompleteTaskPayload, TodoistUpsertTask,
        TodoistUpsertTaskPayload,
    },
    webhooks::WebhookDelivery,
};

type HmacSha256 = Hmac<Sha256>;

/// The key both sides of [`tokens_match`] are run through. Generated once when
/// the process starts and never leaves it, which is what stops a caller working
/// out the digest of their own guess.
static COMPARISON_KEY: LazyLock<[u8; 32]> = LazyLock::new(rand::random);

/// Whether `presented` is `expected`, without saying through timing how much of
/// one matched the other.
///
/// Lives here rather than in each of the two token-carrying webhook types
/// because there is no sense in two copies of a security primitive drifting
/// apart; [`crate::webhooks::honeycomb`] uses this one.
///
/// `==` on two strings stops at the first byte that differs, so how long it
/// takes to answer says how long a prefix the caller guessed right — enough,
/// given a patient caller, to walk a token out one byte at a time. Both sides
/// are therefore run through an HMAC keyed on [`COMPARISON_KEY`] and the
/// resulting digests compared with `Mac::verify_slice`, which is constant-time
/// and is the same primitive the HMAC-signed webhooks (GitHub, Terraform, Grey,
/// Tailscale) verify with. Comparing digests rather than the tokens themselves
/// is what makes the timing useless: without the key nobody can predict what
/// their own guess hashes to, so they cannot steer the comparison.
///
/// Callers refuse an empty configured secret before reaching here, so this is
/// never asked whether two empty strings match.
pub(super) fn tokens_match(expected: &str, presented: &str) -> bool {
    fn keyed() -> HmacSha256 {
        HmacSha256::new_from_slice(COMPARISON_KEY.as_slice())
            .expect("HMAC-SHA256 accepts a key of any length")
    }

    let mut mac = keyed();
    mac.update(expected.as_bytes());
    let expected = mac.finalize().into_bytes();

    let mut mac = keyed();
    mac.update(presented.as_bytes());
    mac.verify_slice(&expected).is_ok()
}

/// What one person asked us to do with their Grafana alerts.
///
/// This carries the token Grafana's contact point sends, because the address on
/// its own does not do the job people assumed it did. The address travels in the
/// URL: it is written to reverse-proxy access logs, to Grafana's own delivery
/// records, and to anything sitting between the two. It is the part of the
/// request most likely to end up somewhere it should not be. The token is
/// carried in the `Authorization` header instead, which those places do not
/// record — this installation's own tracing redacts exactly that header (see
/// [`crate::web::telemetry`]). The two therefore defend against different
/// exposures rather than being two locks on one door, and the token is what
/// survives a leaked URL.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct GrafanaWebhookConfig {
    /// What to call this workflow, so that somebody with alerts from two
    /// Grafana instances can tell which of them filed a task.
    pub name: String,

    /// The credentials set on the Grafana contact point, checked against the
    /// `Authorization` header on each delivery. Deliveries are refused while
    /// this is unset — see [`GrafanaWebhook::handle`] for why.
    ///
    /// A `String` rather than the `Option<String>` this used to be: "unset" and
    /// "set to nothing" were never two different answers, and both have to fail
    /// closed.
    #[serde(default)]
    pub secret: String,

    /// Filter to apply to incoming alerts
    #[serde(default)]
    pub filter: Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: TodoistTarget,
}

impl Display for GrafanaWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "grafana/{}", self.name)
    }
}

fn default_todoist_config() -> crate::publishers::TodoistTarget {
    crate::publishers::TodoistTarget {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

pub struct GrafanaWebhook;

impl GrafanaWebhook {
    /// The credentials out of an `Authorization` header.
    ///
    /// Grafana sends `Authorization: <scheme> <credentials>`, with the scheme
    /// defaulting to `Bearer`. The field on this workflow holds the credentials,
    /// which is the half Grafana's own form asks for, so the scheme is dropped
    /// before comparing — and tolerated when absent, since a contact point can
    /// be set up to send the value bare.
    fn credentials(header: &str) -> &str {
        let header = header.trim();

        match header.split_once(' ') {
            Some((scheme, credentials)) if scheme.eq_ignore_ascii_case("bearer") => {
                credentials.trim_start()
            }
            _ => header,
        }
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

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Files a Todoist task when a Grafana alert starts firing, and completes that
same task when the alert resolves. Both halves are keyed on the alert rule, so
an alert that fires, resolves and fires again reuses one task rather than
leaving a trail of them.

The task links to the alert's dashboard where the payload names one, and its
body carries the alert's `summary` annotation. Priority comes from the alert's
`severity` label: `critical` is raised urgently, then `error`, then `warning`,
and anything else lowest. Labelling your rules is therefore worth doing, or
everything arrives at the same priority.

## Getting the address

Save the workflow first. Its address is generated when it is created and shown
on the workflow afterwards; there is nothing to paste into Grafana until then.

Then, in Grafana, go to **Alerting → Contact points → Add contact point**,
choose the **Webhook** integration, and set its URL to this workflow's address.
Leave the HTTP method as `POST`, and fill in the authorisation — see below.

A contact point on its own delivers nothing. Add a notification policy (or a
rule-level routing) that sends the alerts you care about to it, then use
Grafana's **Test** button to confirm a delivery arrives.

## The authorisation token

Under the contact point's **Optional Webhook settings**, leave **Authorization
Header — Scheme** as `Bearer` and put a long random string into **Authorization
Header — Credentials**. Paste that same string into **Authorization token**
here. They have to match exactly: Grafana sends it on every delivery and we
check it against our copy.

This is not a second lock on the same door as the address. The address travels
in the URL, so it is written into reverse-proxy access logs, into Grafana's own
delivery records, and into anything sitting between the two — it is the part of
the request most likely to end up somewhere it should not be. The token rides
in the `Authorization` header, which those places do not keep, and which this
installation's own request logging redacts. A leaked address therefore does not
leak the token, and the token still refuses the delivery.

**A workflow with no token refuses every alert.** An empty field is not treated
as "skip the check" — that would leave a workflow silently unauthenticated at
exactly the moment somebody forgot to finish setting it up, so a missing token
fails closed instead. It also means the check cannot be turned off by clearing
the box.

## Choosing which alerts to file

The filter runs against the whole delivery and can match on `receiver`,
`status`, `org_id`, `title`, `state`, `message` and `alerts.status`. Leaving it
empty files every alert Grafana routes here, which is reasonable if the
notification policy is already doing the selecting.

```
state == "alerting" && title contains "production"
```

Note that a filter which rejects a delivery rejects it in both directions: if a
firing alert was filed and its resolution does not match the same filter, the
task will not be completed. Filter on things that do not change between the two
— the rule, the folder, the environment — rather than on the status.
"#;

crate::register_job!(GrafanaWebhook);
crate::register_workflow_type!(GrafanaWebhook);

impl crate::workflows::ConfigurableWorkflow for GrafanaWebhook {
    type ConfigType = GrafanaWebhookConfig;

    fn type_id() -> &'static str {
        "grafana"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Grafana".to_string(),
            description:
                "Files a task when a Grafana alert starts firing, and completes it when the alert resolves."
                    .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                // Must name the same partition this job consumes from, or a
                // delivery would be queued somewhere nothing is reading.
                source: "grafana".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(GrafanaWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Production dashboards".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(GrafanaWebhookConfig: secret),
                    "Authorization token",
                    FieldKind::Secret {
                        placeholder: Some("a long random string".into()),
                        generator: true,
                        generator_bytes: 32,
                    },
                )
                .with_help(
                    "The value in the contact point's Authorization Header — Credentials field, under Grafana's Optional Webhook settings. It has to be the same string on both sides. Grafana sends it in a header, which logs and delivery histories do not keep, whereas the address travels in the URL where they do — so this is what still refuses an alert somebody sent because they found the address. Alerts are refused while this is empty.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(GrafanaWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec![
                            "receiver".into(),
                            "status".into(),
                            "org_id".into(),
                            "title".into(),
                            "state".into(),
                            "message".into(),
                            "alerts.status".into(),
                        ],
                    },
                )
                .with_help(
                    "Only file alerts matching this, such as state == \"alerting\". Leave it empty to file every alert.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                GrafanaWebhookConfig,
                project = Some("Life"),
                section = Some("Tasks & Chores")
            ))
            .collect(),
        }
    }
}

impl Job for GrafanaWebhook {
    type JobType = WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/grafana"
    }

    #[instrument("webhooks.grafana.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the payload, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<GrafanaWebhookConfig>(services).await? else {
            return Ok(());
        };

        // Everything below this point happens *before* the payload is parsed, so
        // that a delivery we cannot attribute to Grafana is never interpreted,
        // let alone acted on.
        //
        // A rejection returns `Ok(())` rather than an error: nothing about a
        // wrong token improves by trying again, so raising here would only leave
        // the delivery retrying forever and hiding real failures behind it. The
        // log line is the record that it happened.

        // No token configured means we refuse, rather than accept anything. The
        // alternative — treating an empty token as "skip the check" — would make
        // a workflow silently unauthenticated exactly when somebody forgot to
        // finish setting it up, and a forgotten field should fail closed. It also
        // means the check cannot be neutralised by clearing the box, and it is
        // what the GitHub and Terraform Cloud webhooks do with their own secrets.
        if config.secret.is_empty() {
            warn!(
                "Received a Grafana webhook for a workflow with no authorization token configured; rejecting request."
            );
            return Ok(());
        }

        let Some(authorization) = Self::header(&job.event, "authorization") else {
            warn!("Received a Grafana webhook without an Authorization header; rejecting request.");
            return Ok(());
        };

        if !tokens_match(&config.secret, Self::credentials(authorization)) {
            warn!(
                "Received a Grafana webhook whose Authorization header did not match the configured token; rejecting request."
            );
            return Ok(());
        }

        let event: GrafanaAlertPayload = job.event.json()?;

        // Apply filter to the entire alert payload
        if !config.filter.matches(&event)? {
            info!(
                "Grafana alert '{}' did not match filter; ignoring.",
                event.title
            );
            return Ok(());
        }

        // Process based on alert status
        match event.status {
            GrafanaAlertStatus::Firing => {
                // Create a unique key based on the alert rule URL or title
                let unique_key = event
                    .rule_url
                    .clone()
                    .unwrap_or_else(|| format!("grafana-alert-{}", event.title));

                // Get the first alert for more details
                let first_alert = event.alerts.first();
                let starts_at = first_alert.and_then(|a| a.starts_at);
                let severity = first_alert
                    .and_then(|a| a.labels.get("severity"))
                    .map(|s| s.as_str())
                    .unwrap_or("unknown");

                // Determine priority based on severity label
                let priority = match severity {
                    "critical" => 4,
                    "error" => 3,
                    "warning" => 2,
                    _ => 1,
                };

                let alert_title = event
                    .group_labels
                    .and_then(|l| {
                        l.get("grafana_folder")
                            .or_else(|| l.get("alertname"))
                            .cloned()
                    })
                    .or_else(|| {
                        event
                            .alerts
                            .iter()
                            .filter_map(|a| a.labels.get("rulename"))
                            .next()
                            .cloned()
                    })
                    .unwrap_or("Grafana Alert".to_string());
                let dashboard_url = event
                    .alerts
                    .iter()
                    .filter_map(|a| a.dashboard_url.clone())
                    .next()
                    .or_else(|| event.external_url.clone())
                    .unwrap_or_else(|| "https://grafana.com".into());

                let summary = event
                    .alerts
                    .iter()
                    .filter_map(|a| a.annotations.get("summary").cloned())
                    .collect::<Vec<String>>()
                    .join("\n");

                // Create or update the Todoist task
                TodoistUpsertTask::dispatch(
                    TodoistUpsertTaskPayload {
                        unique_key: unique_key.clone(),
                        title: format!(
                            "[**Grafana Alert**]({dashboard_url}): {alert_title} is unhealthy"
                        ),
                        description: Some(summary),
                        due: starts_at
                            .map(crate::publishers::TodoistDueDate::DateTime)
                            .unwrap_or_else(|| {
                                crate::publishers::TodoistDueDate::DateTime(ctx.scheduled_at())
                            }),
                        priority: Some(priority),
                        config: config.todoist.clone(),
                        ..Default::default()
                    },
                    Some(
                        event
                            .rule_url
                            .clone()
                            .unwrap_or_else(|| event.title.clone())
                            .into(),
                    ),
                    services,
                )
                .await?;

                Ok(())
            }
            GrafanaAlertStatus::Resolved => {
                // Complete the task when the alert is resolved
                let unique_key = event
                    .rule_url
                    .clone()
                    .unwrap_or_else(|| event.title.clone());

                TodoistCompleteTask::dispatch(
                    TodoistCompleteTaskPayload {
                        unique_key,
                        config: config.todoist.clone(),
                    },
                    None,
                    services,
                )
                .await?;

                Ok(())
            }
        }
    }
}

/// Grafana alert webhook payload structure
#[allow(dead_code)]
#[derive(Deserialize)]
pub struct GrafanaAlertPayload {
    /// Name of the contact point (receiver)
    pub receiver: String,
    /// Overall status: "firing" or "resolved"
    pub status: GrafanaAlertStatus,
    /// Organization ID
    #[serde(rename = "orgId")]
    pub org_id: i64,
    /// Alert title
    pub title: String,
    /// Alert state: "alerting", "ok", etc.
    pub state: GrafanaAlertState,
    /// Alert message
    pub message: String,
    /// Grafana base URL
    #[serde(rename = "externalURL")]
    pub external_url: Option<String>,
    /// Direct URL to the alerting rule
    #[serde(rename = "ruleUrl")]
    pub rule_url: Option<String>,
    /// List of individual alerts
    pub alerts: Vec<GrafanaAlert>,
    #[serde(rename = "groupLabels")]
    /// Common labels for the alert group
    pub group_labels: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "commonLabels")]
    pub common_labels: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "commonAnnotations")]
    pub common_annotations: Option<std::collections::HashMap<String, String>>,
}

impl Filterable for GrafanaAlertPayload {
    fn get(&self, key: &str) -> FilterValue<'_> {
        match key {
            "receiver" => self.receiver.as_str().into(),
            "status" => format!("{}", self.status).into(),
            "org_id" => self.org_id.into(),
            "title" => self.title.as_str().into(),
            "state" => format!("{}", self.state).into(),
            "message" => self.message.as_str().into(),
            "alerts.status" => self
                .alerts
                .iter()
                .map(|a| format!("{}", a.status).into())
                .collect::<Vec<_>>()
                .into(),
            _ => FilterValue::Null,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrafanaAlertStatus {
    Firing,
    Resolved,
}

impl Display for GrafanaAlertStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrafanaAlertStatus::Firing => write!(f, "firing"),
            GrafanaAlertStatus::Resolved => write!(f, "resolved"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrafanaAlertState {
    Alerting,
    Ok,
    NoData,
    Paused,
}

impl Display for GrafanaAlertState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrafanaAlertState::Alerting => write!(f, "alerting"),
            GrafanaAlertState::Ok => write!(f, "ok"),
            GrafanaAlertState::NoData => write!(f, "no_data"),
            GrafanaAlertState::Paused => write!(f, "paused"),
        }
    }
}

/// Individual alert within a Grafana alert notification
#[allow(dead_code)]
#[derive(Deserialize)]
pub struct GrafanaAlert {
    /// Alert status: "firing" or "resolved"
    pub status: GrafanaAlertStatus,
    /// Labels associated with the alert
    pub labels: std::collections::HashMap<String, String>,
    /// Annotations associated with the alert
    pub annotations: std::collections::HashMap<String, String>,
    /// When the alert started firing
    #[serde(rename = "startsAt")]
    pub starts_at: Option<DateTime<Utc>>,
    /// When the alert stopped firing (null if still firing)
    #[serde(rename = "endsAt")]
    pub ends_at: Option<DateTime<Utc>>,
    /// URL to the panel/dashboard that generated the alert
    #[serde(rename = "generatorURL")]
    pub generator_url: Option<String>,
    #[serde(rename = "dashboardURL")]
    pub dashboard_url: Option<String>,
    /// Alert values (metrics that triggered the alert)
    #[serde(default)]
    pub values: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::webhooks::WebhookEvent;
    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    const FIRING_PAYLOAD: &str = r#"{
        "receiver": "my-webhook",
        "status": "firing",
        "orgId": 1,
        "title": "[FIRING:1] High CPU usage",
        "state": "alerting",
        "message": "CPU usage is above 90%",
        "externalURL": "http://localhost:3000",
        "ruleUrl": "http://localhost:3000/alerting/rule/1",
        "alerts": [
            {
                "status": "firing",
                "labels": {
                    "severity": "critical",
                    "instance": "localhost:9090"
                },
                "annotations": {
                    "summary": "High CPU usage detected"
                },
                "startsAt": "2025-12-11T22:05:00Z",
                "endsAt": null,
                "generatorURL": "http://localhost:3000/d/xyz?viewPanel=2",
                "values": {
                    "cpu_usage": 95
                }
            }
        ]
    }"#;

    const RESOLVED_PAYLOAD: &str = r#"{
        "receiver": "my-webhook",
        "status": "resolved",
        "orgId": 1,
        "title": "[RESOLVED] High CPU usage",
        "state": "ok",
        "message": "CPU usage has returned to normal",
        "externalURL": "http://localhost:3000",
        "ruleUrl": "http://localhost:3000/alerting/rule/1",
        "alerts": [
            {
                "status": "resolved",
                "labels": {
                    "severity": "critical",
                    "instance": "localhost:9090"
                },
                "annotations": {
                    "summary": "High CPU usage detected"
                },
                "startsAt": "2025-12-11T22:05:00Z",
                "endsAt": "2025-12-11T22:15:00Z",
                "generatorURL": "http://localhost:3000/d/xyz?viewPanel=2",
                "values": {
                    "cpu_usage": 60
                }
            }
        ]
    }"#;

    /// The token these tests pretend was set on both this workflow and the
    /// Grafana contact point.
    const TOKEN: &str = "a-long-random-string";

    /// A workflow authorised with [`TOKEN`].
    fn config() -> serde_json::Value {
        serde_json::json!({ "name": "Production", "secret": TOKEN })
    }

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "grafana".into(),
                config,
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id
    }

    /// A delivery carrying the `Authorization` header Grafana would have sent.
    fn delivery(workflow: automate_api::WorkflowId, body: impl Into<String>) -> WebhookDelivery {
        delivery_with(
            workflow,
            body,
            &[("Authorization", &format!("Bearer {TOKEN}"))],
        )
    }

    /// A delivery carrying whatever headers the test wants, for the ones about
    /// what happens when the token is wrong, absent or unmatched.
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

    async fn run(
        services: &(impl Services + Send + Sync + Clone + 'static),
        delivery: &WebhookDelivery,
    ) -> Result<(), human_errors::Error> {
        GrafanaWebhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                delivery,
            )
            .await
    }

    async fn queued(
        services: &(impl Services + Send + Sync + 'static),
        partition: &'static str,
    ) -> Vec<crate::db::PeekedMessage<serde_json::Value>> {
        services
            .queue()
            .peek(partition, 10)
            .await
            .expect("peek the todoist queue")
    }

    #[tokio::test]
    async fn a_firing_alert_opens_a_task_for_it() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, FIRING_PAYLOAD))
            .await
            .expect("Webhook should handle firing alert");

        assert_eq!(queued(&services, "todoist/upsert-task").await.len(), 1);
    }

    #[tokio::test]
    async fn a_resolved_alert_completes_the_task_it_filed() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, RESOLVED_PAYLOAD))
            .await
            .expect("Webhook should handle resolved alert");

        assert_eq!(queued(&services, "todoist/complete-task").await.len(), 1);
    }

    #[tokio::test]
    async fn a_body_that_is_not_grafanas_is_refused() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let result = run(&services, &delivery(workflow, r#"{"invalid json"#)).await;

        assert!(result.is_err(), "Webhook should reject invalid JSON");
    }

    #[tokio::test]
    async fn an_alert_the_workflows_filter_rejects_files_nothing() {
        // The filter now belongs to the workflow rather than the installation,
        // so this is also what proves the handler reads the stored record.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Production",
                "secret": TOKEN,
                "filter": "receiver == \"someone-elses-webhook\"",
            }),
        )
        .await;

        run(&services, &delivery(workflow, FIRING_PAYLOAD))
            .await
            .unwrap();

        assert!(
            queued(&services, "todoist/upsert-task").await.is_empty(),
            "an alert the owner asked to ignore should not have filed anything",
        );
    }

    #[tokio::test]
    async fn a_delivery_for_a_workflow_that_is_gone_stops_there() {
        // Deliveries queue behind one another, so a workflow can be deleted
        // while one of its own is still waiting to run.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        WorkflowStore::new(&services)
            .with_index(&services)
            .delete(workflow)
            .await
            .unwrap();

        run(&services, &delivery(workflow, FIRING_PAYLOAD))
            .await
            .expect("a deleted workflow should not fail the delivery");

        assert!(queued(&services, "todoist/upsert-task").await.is_empty());
    }

    #[test]
    fn an_alert_exposes_the_fields_the_filter_editor_offers() {
        let alert = GrafanaAlertPayload {
            receiver: "test-receiver".to_string(),
            status: GrafanaAlertStatus::Firing,
            org_id: 123,
            title: "Test Alert".to_string(),
            state: GrafanaAlertState::Alerting,
            message: "Test message".to_string(),
            external_url: Some("https://grafana.example.com".to_string()),
            rule_url: Some("https://grafana.example.com/rule/1".to_string()),
            alerts: vec![],
            group_labels: None,
            common_labels: None,
            common_annotations: None,
        };

        // Test Filterable trait implementation
        assert_eq!(
            alert.get("receiver"),
            FilterValue::from("test-receiver".to_string())
        );
        assert_eq!(alert.get("status"), FilterValue::from("firing".to_string()));
        assert_eq!(alert.get("org_id"), FilterValue::from(123));
        assert_eq!(
            alert.get("title"),
            FilterValue::from("Test Alert".to_string())
        );
        assert_eq!(
            alert.get("state"),
            FilterValue::from("alerting".to_string())
        );
        assert_eq!(
            alert.get("message"),
            FilterValue::from("Test message".to_string())
        );
        assert_eq!(alert.get("unknown_field"), FilterValue::Null);
    }

    #[test]
    fn deliveries_are_queued_where_this_workflow_reads_them() {
        // The trigger decides where a configuration is stored and the job
        // decides where deliveries are queued. A mismatch between the two is a
        // workflow that saves happily and never runs.
        use crate::workflows::ConfigurableWorkflow;

        assert_eq!(
            GrafanaWebhook::descriptor().trigger.partition(),
            <GrafanaWebhook as Job>::partition(),
        );
    }

    #[test]
    fn the_token_is_read_out_of_the_header_whether_or_not_grafana_named_a_scheme() {
        // Grafana's form collects the scheme and the credentials separately and
        // sends them joined, so the field here holds the credentials half. A
        // contact point configured without a scheme sends the value bare, and
        // that has to be the same token rather than a different one.
        assert_eq!(GrafanaWebhook::credentials("Bearer hunter2"), "hunter2");
        assert_eq!(GrafanaWebhook::credentials("bearer hunter2"), "hunter2");
        assert_eq!(GrafanaWebhook::credentials("hunter2"), "hunter2");
    }

    #[test]
    fn a_token_only_matches_itself() {
        assert!(tokens_match(TOKEN, TOKEN));
        assert!(!tokens_match(TOKEN, "somebody-elses-token"));
        assert!(
            !tokens_match(TOKEN, &TOKEN[..TOKEN.len() - 1]),
            "a prefix of the token is not the token, however long it is",
        );
    }

    #[tokio::test]
    async fn an_alert_carrying_the_wrong_token_files_nothing() {
        // Each workflow carries its own token, so an alert authorised for
        // somebody else's must not be acted on by this one.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let job = delivery_with(
            workflow,
            FIRING_PAYLOAD,
            &[("Authorization", "Bearer somebody-elses-token")],
        );

        run(&services, &job)
            .await
            .expect("a misauthorised alert should be refused without erroring");

        assert!(queued(&services, "todoist/upsert-task").await.is_empty());
    }

    #[tokio::test]
    async fn an_alert_with_no_authorization_header_at_all_files_nothing() {
        // Anybody can post to a URL, and the URL is the part of the request most
        // likely to have leaked. Without the header there is nothing to check,
        // and "nothing to check" is not the same as "checks out".
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery_with(workflow, FIRING_PAYLOAD, &[]))
            .await
            .expect("an unauthorised alert should be refused without erroring");

        assert!(queued(&services, "todoist/upsert-task").await.is_empty());
    }

    #[tokio::test]
    async fn an_alert_carrying_a_near_miss_of_the_token_files_nothing() {
        // A near miss is the interesting case: somebody probing for the token
        // works by getting closer to it, so a value that shares a long prefix
        // with the real one, or differs only in case or length, has to be as
        // rejected as a value that shares nothing.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        for near_miss in [
            &TOKEN[..TOKEN.len() - 1],
            &format!("{TOKEN}x"),
            &TOKEN.to_uppercase(),
            "",
        ] {
            let job = delivery_with(
                workflow,
                FIRING_PAYLOAD,
                &[("Authorization", &format!("Bearer {near_miss}"))],
            );

            run(&services, &job)
                .await
                .expect("a near miss should be refused without erroring");

            assert!(
                queued(&services, "todoist/upsert-task").await.is_empty(),
                "'{near_miss}' is not the configured token and must not be treated as it",
            );
        }
    }

    #[tokio::test]
    async fn alerts_are_refused_while_the_workflow_has_no_token_configured() {
        // A half-finished workflow should file nothing rather than file whatever
        // anybody who found the URL cares to post. An empty field means "cannot
        // be verified", not "need not be verified" — and in particular an empty
        // token must not match an empty header.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({ "name": "Production", "secret": "" }),
        )
        .await;

        run(&services, &delivery(workflow, FIRING_PAYLOAD))
            .await
            .expect("an unverifiable alert should be refused without erroring");
        run(
            &services,
            &delivery_with(workflow, FIRING_PAYLOAD, &[("Authorization", "")]),
        )
        .await
        .expect("an empty header should not satisfy an empty token");

        assert!(queued(&services, "todoist/upsert-task").await.is_empty());
    }

    #[tokio::test]
    async fn the_authorization_header_is_recognised_whatever_case_it_arrives_in() {
        // HTTP header names are case-insensitive and whatever proxy sits in
        // front of us is free to renormalise them, so a lowercase header must
        // not read as a missing one.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let job = delivery_with(
            workflow,
            FIRING_PAYLOAD,
            &[("authorization", &format!("Bearer {TOKEN}"))],
        );

        run(&services, &job)
            .await
            .expect("a correctly authorised alert should be processed");

        assert_eq!(queued(&services, "todoist/upsert-task").await.len(), 1);
    }
}
