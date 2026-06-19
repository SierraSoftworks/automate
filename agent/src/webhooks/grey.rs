//! Webhook handler for [Grey](https://github.com/SierraSoftworks/grey) state-change notifications.
//!
//! Grey delivers a signed JSON document whenever a probe or cron changes state. We turn an
//! *unhealthy* transition into a Todoist task describing the entity's current state (with
//! markdown context for an operator), and complete that task once the entity recovers.
//!
//! The handler mirrors the [`super::tailscale`] (HMAC signature verification) and
//! [`super::grafana`] (Todoist upsert on firing / complete on resolve) handlers:
//!
//! * The payload is authenticated with the same Tailscale-style HMAC-SHA256 scheme Grey signs
//!   with — `Grey-Webhook-Signature: t=<unix-seconds>,v1=<hex>` over `"<timestamp>.<body>"`.
//! * Each monitor is correlated by a stable idempotency key (`grey/<type>/<name>`), so a flapping
//!   monitor reuses the same task rather than spamming the operator with new ones.
//! * Recovery completes the task on a one-hour delay, so a monitor that briefly recovers before
//!   failing again does not churn the task. A fresh failure cancels any pending completion.

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::{
    prelude::*,
    publishers::{
        TodoistCompleteTask, TodoistCompleteTaskPayload, TodoistDueDate, TodoistUpsertTask,
        TodoistUpsertTaskPayload,
    },
};

type HmacSha256 = Hmac<Sha256>;

/// How long to wait before completing a recovered monitor's task, debouncing flapping monitors so a
/// brief recovery followed by another failure does not churn the task.
const RECOVERY_COMPLETION_DELAY: chrono::Duration = chrono::Duration::hours(1);

#[derive(Clone, Deserialize, Default)]
pub struct GreyWebhookConfig {
    /// Shared secret used to verify the `Grey-Webhook-Signature` HMAC. When empty, signature
    /// verification is skipped (only safe when the endpoint is otherwise trusted).
    #[serde(default)]
    pub secret: String,

    /// Optional base URL of the Grey status page, used to link the Todoist task back to Grey.
    #[serde(default)]
    pub dashboard_url: Option<String>,

    /// Filter applied to incoming events. The same fields Grey exposes to its own webhook filters
    /// are available here (`event`, `entity.*`, `state.*`).
    #[serde(default)]
    pub filter: crate::filter::Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: crate::config::TodoistConfig,
}

fn default_todoist_config() -> crate::config::TodoistConfig {
    crate::config::TodoistConfig {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct GreyWebhook;

impl GreyWebhook {
    /// Parses Grey's `t=<unix-seconds>,v1=<hex>` signature header into its timestamp and raw bytes.
    fn parse_signature(header: &str) -> Result<(DateTime<Utc>, Vec<u8>), human_errors::Error> {
        let mut timestamp = None;
        let mut signature = None;

        for (key, value) in header.split(',').filter_map(|s| s.split_once('=')) {
            match key {
                "t" => timestamp = Some(value),
                "v1" => signature = Some(value),
                _ => {} // Ignore unknown fields
            }
        }

        match (timestamp, signature) {
            (Some(timestamp), Some(signature)) => {
                let timestamp = timestamp
                    .parse()
                    .ok()
                    .and_then(|ts| DateTime::from_timestamp(ts, 0))
                    .ok_or_else(|| {
                        human_errors::user(
                            "The timestamp in the Grey-Webhook-Signature header is invalid.",
                            &[
                                "Ensure that you are only sending Grey webhooks to this endpoint.",
                                "Check that the webhook is configured correctly in your Grey configuration.",
                            ],
                        )
                    })?;

                let signature = hex::decode(signature).or_user_err(&[
                    "The signature in the Grey-Webhook-Signature header is not valid hex.",
                    "Ensure that you are only sending Grey webhooks to this endpoint.",
                    "Check that the webhook is configured correctly in your Grey configuration.",
                ])?;

                Ok((timestamp, signature))
            }
            _ => Err(human_errors::user(
                "The Grey-Webhook-Signature header did not contain a valid signature.",
                &[
                    "Ensure that you are only sending Grey webhooks to this endpoint.",
                    "Check that the webhook is configured correctly in your Grey configuration.",
                ],
            )),
        }
    }

    /// Verifies the Grey webhook signature.
    ///
    /// Grey signs webhooks using the scheme [documented for Tailscale](https://tailscale.com/kb/1213/webhooks):
    /// HMAC-SHA256 over `"<timestamp>.<body>"`, with the signature carried in the
    /// `Grey-Webhook-Signature` header as `t=<timestamp>,v1=<hex_signature>`.
    ///
    /// The `now` parameter is the point in time against which the signature timestamp is validated.
    /// This should be the time at which the request was originally received (rather than the current
    /// time) so that retries of a previously received request continue to validate successfully.
    fn verify_signature(
        secret: &str,
        body: &str,
        signature_header: &str,
        now: DateTime<Utc>,
    ) -> Result<(), human_errors::Error> {
        let (timestamp, expected_signature) = Self::parse_signature(signature_header)?;

        if (timestamp - now).abs() > chrono::Duration::minutes(5) {
            return Err(human_errors::user(
                format!(
                    "The Grey webhook signature timestamp is too old or too far in the future (got {})",
                    timestamp
                ),
                &[
                    "Ensure that the system clock on this server is accurate.",
                    "Check that the webhook is configured correctly in your Grey configuration.",
                ],
            ));
        }

        // Create the string to sign: <timestamp>.<body>
        let string_to_sign = format!("{}.{}", timestamp.timestamp(), body);

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).wrap_user_err(
            "Failed to create HMAC instance with the provided secret.",
            &[
                "Ensure that you have provided a valid webhooks.grey.secret in your configuration.",
                "Ensure that the configured webhooks.grey.secret matches the secret on the Grey webhook.",
            ],
        )?;

        mac.update(string_to_sign.as_bytes());

        mac.verify_slice(&expected_signature).wrap_user_err(
            "Webhook signature verification failed (signatures did not match).".to_string(),
            &["Ensure that the configured webhooks.grey.secret matches the secret on the Grey webhook."],
        )?;

        Ok(())
    }
}

crate::register_job!(GreyWebhook);

impl Job for GreyWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/grey"
    }

    #[instrument("webhooks.grey.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Validate the Grey webhook signature header, exactly as for Tailscale.
        let secret = &services.config().webhooks.grey.secret;

        if !secret.is_empty() {
            // HTTP headers are case-insensitive, so search for the header case-insensitively.
            let signature = job
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("grey-webhook-signature"))
                .map(|(_, value)| value.as_str());

            if let Some(signature) = signature {
                // Validate against the time the request was originally received (the message's
                // scheduled time) so that retries of a previously received webhook still validate.
                if let Err(err) =
                    Self::verify_signature(secret, &job.body, signature, ctx.scheduled_at())
                {
                    warn!(
                        "Failed to verify Grey webhook signature, rejecting request: {}",
                        err
                    );
                    return Ok(());
                }
            } else {
                warn!(
                    "Received Grey webhook without signature, but secret is configured; rejecting request."
                );
                return Ok(());
            }
        } else {
            debug!("No Grey webhook secret configured; skipping signature verification.");
        }

        let event: GreyWebhookEvent = job.json()?;

        if !services.config().webhooks.grey.filter.matches(&event)? {
            info!(
                "Grey event for {} '{}' did not match filter; ignoring.",
                event.entity.entity_type, event.entity.name
            );
            return Ok(());
        }

        let config = services.config().webhooks.grey.todoist.clone();

        // A stable key per monitor so a flapping entity reuses a single task rather than creating a
        // fresh notification each time it changes state.
        let unique_key = event.unique_key();

        if event.state.healthy {
            // The monitor has recovered. Complete the task on a delay so a brief recovery that is
            // immediately followed by another failure does not churn the task. Re-dispatching with
            // the same idempotency key resets the timer on each recovery.
            info!(
                "Grey {} '{}' recovered ({} -> {}); scheduling task completion.",
                event.entity.entity_type,
                event.entity.name,
                event.state.previous,
                event.state.current
            );

            TodoistCompleteTask::dispatch_delayed(
                TodoistCompleteTaskPayload {
                    unique_key: unique_key.clone(),
                    config,
                },
                Some(unique_key.into()),
                RECOVERY_COMPLETION_DELAY,
                services,
            )
            .await?;
        } else {
            // The monitor is unhealthy. Cancel any pending recovery completion first, so a flap of
            // recover -> fail does not let an in-flight completion close a task that is unhealthy
            // again, then create/refresh the operator's task.
            services
                .queue()
                .purge(TodoistCompleteTask::partition(), unique_key.clone())
                .await?;

            TodoistUpsertTask::dispatch(
                TodoistUpsertTaskPayload {
                    unique_key: unique_key.clone(),
                    title: event
                        .task_title(services.config().webhooks.grey.dashboard_url.as_deref()),
                    description: Some(event.task_description()),
                    due: event
                        .state
                        .since
                        .map(TodoistDueDate::DateTime)
                        .unwrap_or_else(|| TodoistDueDate::DateTime(ctx.scheduled_at())),
                    priority: Some(event.priority()),
                    config,
                    ..Default::default()
                },
                Some(unique_key.into()),
                services,
            )
            .await?;
        }

        Ok(())
    }
}

/// A Grey `probe.state_changed` / `cron.state_changed` webhook payload.
///
/// This mirrors the wire shape of `grey_api::WebhookEvent` (see Grey's `docs/guide/webhooks.md`),
/// carrying only the fields we read. The full `probe`/`cron` snapshots are kept as raw JSON so we
/// can surface a little extra context without coupling to Grey's internal types.
#[allow(dead_code)]
#[derive(Deserialize)]
struct GreyWebhookEvent {
    #[serde(default)]
    version: String,
    id: String,
    event: String,
    timestamp: DateTime<Utc>,
    entity: GreyEntity,
    state: GreyState,
    #[serde(default)]
    probe: Option<serde_json::Value>,
    #[serde(default)]
    cron: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GreyEntity {
    #[serde(rename = "type")]
    entity_type: String,
    name: String,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct GreyState {
    current: String,
    previous: String,
    healthy: bool,
    was_healthy: bool,
    #[serde(default)]
    since: Option<DateTime<Utc>>,
    #[serde(default)]
    availability: Option<f64>,
}

impl GreyWebhookEvent {
    /// A stable per-monitor key (`grey/<type>/<name>`) used both to correlate the Todoist task and
    /// as the queue idempotency key for the create/complete jobs.
    fn unique_key(&self) -> String {
        format!("grey/{}/{}", self.entity.entity_type, self.entity.name)
    }

    /// A human-friendly label for the entity type (`Probe` / `Cron`), title-cased for display.
    fn entity_label(&self) -> String {
        let mut chars = self.entity.entity_type.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => "Monitor".to_string(),
        }
    }

    /// The Todoist task title, linking back to the Grey status page when one is configured.
    fn task_title(&self, dashboard_url: Option<&str>) -> String {
        let body = format!(
            "{} `{}` is {}",
            self.entity_label(),
            self.entity.name,
            self.state.current
        );

        match dashboard_url {
            Some(url) if !url.is_empty() => format!("[**Grey**]({url}): {body}"),
            _ => format!("**Grey**: {body}"),
        }
    }

    /// A markdown description giving an operator the context needed to triage the alert: the
    /// transition, when it happened, availability, tags, and the most recent failure detail.
    fn task_description(&self) -> String {
        let mut lines = vec![
            format!(
                "**{} `{}`** changed from **{}** to **{}**.",
                self.entity_label(),
                self.entity.name,
                self.state.previous,
                self.state.current
            ),
            String::new(),
        ];

        if let Some(since) = self.state.since {
            lines.push(format!("- **Since:** {}", since.to_rfc3339()));
        }

        if let Some(availability) = self.state.availability {
            lines.push(format!("- **Availability:** {availability:.2}%"));
        }

        if !self.entity.tags.is_empty() {
            // Sort the tags so the description (and thus the upsert hash) is deterministic.
            let mut tags: Vec<_> = self.entity.tags.iter().collect();
            tags.sort();
            let rendered = tags
                .into_iter()
                .map(|(key, value)| format!("`{key}={value}`"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("- **Tags:** {rendered}"));
        }

        if let Some(detail) = self.failure_detail() {
            lines.push(String::new());
            lines.push(format!("**Latest detail:** {detail}"));
        }

        lines.push(String::new());
        lines.push(format!("_Event `{}` (schema {})._", self.id, self.version));

        lines.join("\n")
    }

    /// The most recent failure detail from the embedded snapshot: the latest probe history bucket's
    /// message, or a cron's last check-in. Returns `None` when nothing useful is available.
    fn failure_detail(&self) -> Option<String> {
        if let Some(probe) = &self.probe {
            let message = probe
                .get("history")?
                .as_array()?
                .last()?
                .get("message")?
                .as_str()?
                .trim();

            if !message.is_empty() {
                return Some(message.to_string());
            }
        }

        if let Some(cron) = &self.cron {
            let checkin = cron.get("last_checkin")?;
            let status = checkin.get("status").and_then(|s| s.as_str());
            let message = checkin
                .get("message")
                .and_then(|m| m.as_str())
                .map(str::trim)
                .filter(|m| !m.is_empty());

            return match (status, message) {
                (Some(status), Some(message)) => {
                    Some(format!("last check-in `{status}`: {message}"))
                }
                (Some(status), None) => Some(format!("last check-in `{status}`")),
                (None, Some(message)) => Some(message.to_string()),
                (None, None) => None,
            };
        }

        None
    }

    /// The Todoist priority for an unhealthy monitor, escalating the most disruptive states.
    fn priority(&self) -> i32 {
        match self.state.current.as_str() {
            // Probe down, cron failed, or a run that never started are the most urgent.
            "failing" | "failed" | "missing" => 4,
            // An overrunning ("stuck") run is concerning but the job is at least alive.
            "stuck" => 3,
            _ => 3,
        }
    }
}

impl Filterable for GreyWebhookEvent {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        use crate::filter::FilterValue;

        match key {
            "event" => self.event.as_str().into(),
            "entity.type" | "entity.kind" => self.entity.entity_type.as_str().into(),
            "entity.name" => self.entity.name.as_str().into(),
            "state.current" => self.state.current.as_str().into(),
            "state.previous" => self.state.previous.as_str().into(),
            "state.healthy" => FilterValue::Bool(self.state.healthy),
            "state.was_healthy" => FilterValue::Bool(self.state.was_healthy),
            "state.availability" => self
                .state
                .availability
                .map(FilterValue::Number)
                .unwrap_or(FilterValue::Null),
            k if k.starts_with("entity.tags.") => self
                .entity
                .tags
                .get(&k["entity.tags.".len()..])
                .map(|v| v.as_str().into())
                .unwrap_or(FilterValue::Null),
            k if k.starts_with("tags.") => self
                .entity
                .tags
                .get(&k["tags.".len()..])
                .map(|v| v.as_str().into())
                .unwrap_or(FilterValue::Null),
            _ => FilterValue::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhooks::WebhookEvent;
    use std::collections::HashMap;

    /// Generates a valid Grey signature (`t=<timestamp>,v1=<hex>`) for testing.
    fn generate_signature(secret: &str, timestamp: i64, body: &str) -> String {
        let string_to_sign = format!("{}.{}", timestamp, body);
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(string_to_sign.as_bytes());
        let hex_sig = hex::encode(mac.finalize().into_bytes());
        format!("t={},v1={}", timestamp, hex_sig)
    }

    fn probe_event(name: &str, healthy: bool) -> String {
        let (current, previous) = if healthy {
            ("passing", "failing")
        } else {
            ("failing", "passing")
        };

        format!(
            r#"{{
                "version": "v1",
                "id": "evt-1",
                "event": "probe.state_changed",
                "timestamp": "2026-06-19T12:00:00Z",
                "entity": {{ "type": "probe", "name": "{name}", "tags": {{ "service": "Web" }} }},
                "state": {{
                    "current": "{current}",
                    "previous": "{previous}",
                    "healthy": {healthy},
                    "was_healthy": {was_healthy},
                    "since": "2026-06-19T11:59:30Z",
                    "availability": 98.7
                }},
                "probe": {{ "history": [{{ "pass": false, "message": "HTTP 503" }}] }}
            }}"#,
            was_healthy = !healthy
        )
    }

    #[test]
    fn test_verify_signature_valid() {
        let secret = "test_secret_key";
        let timestamp = Utc::now().timestamp();
        let body = probe_event("web.prod", false);
        let signature = generate_signature(secret, timestamp, &body);

        GreyWebhook::verify_signature(secret, &body, &signature, Utc::now())
            .expect("Valid signature should verify successfully");
    }

    #[test]
    fn test_verify_signature_valid_on_retry() {
        // A retry validates against the original receipt time, not the (much later) current time.
        let secret = "test_secret_key";
        let received_at = Utc::now() - chrono::Duration::hours(6);
        let timestamp = received_at.timestamp();
        let body = probe_event("web.prod", false);
        let signature = generate_signature(secret, timestamp, &body);

        assert!(
            GreyWebhook::verify_signature(secret, &body, &signature, Utc::now()).is_err(),
            "Signature should be rejected when validated against the current time"
        );
        GreyWebhook::verify_signature(secret, &body, &signature, received_at)
            .expect("Signature should verify against the original receipt time on retry");
    }

    #[test]
    fn test_verify_signature_wrong_secret() {
        let timestamp = Utc::now().timestamp();
        let body = probe_event("web.prod", false);
        let signature = generate_signature("wrong_secret", timestamp, &body);

        assert!(
            GreyWebhook::verify_signature("test_secret_key", &body, &signature, Utc::now())
                .is_err(),
            "Signature with wrong secret should fail verification"
        );
    }

    #[test]
    fn test_verify_signature_tampered_body() {
        let secret = "test_secret_key";
        let timestamp = Utc::now().timestamp();
        let original = probe_event("web.prod", false);
        let tampered = probe_event("web.staging", false);
        let signature = generate_signature(secret, timestamp, &original);

        assert!(
            GreyWebhook::verify_signature(secret, &tampered, &signature, Utc::now()).is_err(),
            "Tampered body should fail verification"
        );
    }

    #[test]
    fn test_verify_signature_invalid_format() {
        let secret = "test_secret_key";
        let body = probe_event("web.prod", false);

        assert!(
            GreyWebhook::verify_signature(secret, &body, "not_a_valid_format", Utc::now()).is_err(),
            "Invalid format should fail"
        );
    }

    #[test]
    fn test_unique_key_is_stable_per_monitor() {
        let failing: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", false)).unwrap();
        let recovered: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", true)).unwrap();

        // The key identifies the monitor, not its current state, so the failing and recovered
        // events for one probe correlate to the same Todoist task.
        assert_eq!(failing.unique_key(), "grey/probe/web.prod");
        assert_eq!(failing.unique_key(), recovered.unique_key());
    }

    #[test]
    fn test_task_title_links_when_dashboard_configured() {
        let event: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", false)).unwrap();

        assert_eq!(
            event.task_title(Some("https://grey.example.com")),
            "[**Grey**](https://grey.example.com): Probe `web.prod` is failing"
        );
        assert_eq!(
            event.task_title(None),
            "**Grey**: Probe `web.prod` is failing"
        );
    }

    #[test]
    fn test_task_description_includes_failure_detail_and_tags() {
        let event: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", false)).unwrap();
        let description = event.task_description();

        assert!(description.contains("changed from **passing** to **failing**"));
        assert!(description.contains("- **Availability:** 98.70%"));
        assert!(description.contains("`service=Web`"));
        assert!(description.contains("**Latest detail:** HTTP 503"));
    }

    #[test]
    fn test_cron_failure_detail_uses_last_checkin() {
        let body = r#"{
            "id": "evt-2",
            "event": "cron.state_changed",
            "timestamp": "2026-06-19T12:00:00Z",
            "entity": { "type": "cron", "name": "backup", "tags": {} },
            "state": { "current": "failed", "previous": "succeeded", "healthy": false, "was_healthy": true },
            "cron": { "last_checkin": { "status": "failed", "message": "exit code 1" } }
        }"#;
        let event: GreyWebhookEvent = serde_json::from_str(body).unwrap();

        assert_eq!(event.entity_label(), "Cron");
        assert_eq!(event.priority(), 4);
        assert_eq!(
            event.failure_detail().as_deref(),
            Some("last check-in `failed`: exit code 1")
        );
    }

    #[test]
    fn test_filter_exposes_grey_fields() {
        let event: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", false)).unwrap();

        assert!(
            Filter::new(r#"entity.type == "probe""#)
                .unwrap()
                .matches(&event)
                .unwrap()
        );
        assert!(
            Filter::new("state.healthy == false")
                .unwrap()
                .matches(&event)
                .unwrap()
        );
        assert!(
            Filter::new(r#"tags.service == "Web""#)
                .unwrap()
                .matches(&event)
                .unwrap()
        );
        assert!(
            !Filter::new(r#"entity.type == "cron""#)
                .unwrap()
                .matches(&event)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_grey_webhook_unhealthy_creates_task() {
        let services = crate::testing::mock_services().await.unwrap();
        let webhook = GreyWebhook;

        let event = WebhookEvent {
            body: probe_event("web.prod", false),
            query: String::new(),
            headers: HashMap::new(),
        };

        let result = webhook
            .handle(JobContext::new(services, Utc::now(), None, None), &event)
            .await;
        assert!(result.is_ok(), "Webhook should handle an unhealthy event");
    }

    #[tokio::test]
    async fn test_grey_webhook_recovered_completes_task() {
        let services = crate::testing::mock_services().await.unwrap();
        let webhook = GreyWebhook;

        let event = WebhookEvent {
            body: probe_event("web.prod", true),
            query: String::new(),
            headers: HashMap::new(),
        };

        let result = webhook
            .handle(JobContext::new(services, Utc::now(), None, None), &event)
            .await;
        assert!(result.is_ok(), "Webhook should handle a recovery event");
    }

    #[tokio::test]
    async fn test_grey_webhook_invalid_json() {
        let services = crate::testing::mock_services().await.unwrap();
        let webhook = GreyWebhook;

        let event = WebhookEvent {
            body: r#"{"invalid json"#.to_string(),
            query: String::new(),
            headers: HashMap::new(),
        };

        let result = webhook
            .handle(JobContext::new(services, Utc::now(), None, None), &event)
            .await;
        assert!(result.is_err(), "Webhook should reject invalid JSON");
    }
}
