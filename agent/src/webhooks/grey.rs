//! Webhook handler for [Grey](https://github.com/SierraSoftworks/grey) state-change notifications.
//!
//! Grey delivers a signed JSON document whenever a probe or cron changes state. Rather than
//! surfacing every transition immediately, we run a small debounce state machine so an operator is
//! only interrupted for incidents that stick, and so a single Todoist task tells a coherent story
//! as an incident progresses from unhealthy, through recovering, to recovered.
//!
//! Per-monitor state is tracked in the [`GREY_FAILURES_PARTITION`] key/value table and drives the
//! following transitions, all correlated by a stable `grey/<type>/<name>` key:
//!
//! * **Unhealthy** — we schedule the operator's Todoist task [`ALERT_DELAY`] into the future rather
//!   than creating it immediately, and record when the incident first went unhealthy (preserved
//!   across relapses so impact is measured from the first sign of failure). If the monitor recovers
//!   before the delay elapses the pending task is purged, so a brief blip never surfaces an alert.
//! * **Healthy** — if a task actually surfaced we immediately flip it to *recovering* at a reduced
//!   priority and schedule a deferred *recovered* update [`RECOVERY_WINDOW`] out, which stamps the
//!   task with the total impact time. (Grey already debounces recovery internally for 5m, so a
//!   healthy report is a strong signal that recovery is genuine.) If no task ever surfaced we simply
//!   forget the incident.
//! * **Unhealthy again within [`RECOVERY_WINDOW`]** — the deferred *recovered* update is cancelled
//!   and the task is re-escalated to unhealthy immediately, since we already know an operator is
//!   watching it.
//!
//! Signatures are verified exactly as for [`super::tailscale`]: HMAC-SHA256 over
//! `"<timestamp>.<body>"`, carried in the `Grey-Webhook-Signature: t=<unix-seconds>,v1=<hex>`
//! header.

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    prelude::*,
    publishers::{
        TodoistDueDate, TodoistUpsertTask, TodoistUpsertTaskPayload, TodoistUpsertTaskState,
    },
};

type HmacSha256 = Hmac<Sha256>;

/// How long to wait before surfacing a Todoist task for a newly-unhealthy monitor. This gives a
/// flapping monitor time to settle before an operator is alerted; a recovery received within the
/// window purges the pending task so a brief blip never surfaces at all.
const ALERT_DELAY: chrono::Duration = chrono::Duration::minutes(5);

/// How long after a monitor first reports healthy we keep the recovery "provisional". While inside
/// this window the task is shown as *recovering*; once it elapses without another failure the task
/// is stamped as fully *recovered*. A failure received within the window re-escalates the existing
/// task instead of being treated as a brand-new incident.
const RECOVERY_WINDOW: chrono::Duration = chrono::Duration::hours(1);

/// The Todoist priority applied while a monitor is recovering or has recovered. It sits below every
/// unhealthy priority (see [`GreyWebhookEvent::priority`]) so the task visibly de-escalates as the
/// incident resolves.
const RECOVERING_PRIORITY: i32 = 2;

/// The key/value partition tracking the failure and recovery state of each Grey monitor, keyed by
/// [`GreyWebhookEvent::unique_key`].
const GREY_FAILURES_PARTITION: &str = "grey/failures";

/// Persisted failure/recovery state for a single Grey monitor, stored in
/// [`GREY_FAILURES_PARTITION`].
#[derive(Clone, Serialize, Deserialize)]
struct GreyFailureRecord {
    /// The time of the *first* unhealthy report in the current incident, so the total impact time is
    /// measured from the first sign of failure rather than the most recent flap. It is preserved
    /// across relapses inside the recovery window and only reset once a genuinely new incident
    /// begins (see [`GreyWebhookEvent`] handling).
    first_unhealthy_at: DateTime<Utc>,

    /// When the monitor entered the *recovering* state — the time of the healthy event that began
    /// the current recovery window — or `None` when the monitor is not currently recovering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovering_since: Option<DateTime<Utc>>,
}

/// Formats a duration as a compact, human-readable string (e.g. `1h 5m`, `12m`, `45s`, `0s`).
/// Sub-minute components are only shown when the duration is under an hour, keeping longer spans
/// tidy. Negative durations are clamped to `0s`.
fn format_duration(duration: chrono::Duration) -> String {
    let total_seconds = duration.num_seconds().max(0);
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 && hours == 0 {
        parts.push(format!("{seconds}s"));
    }
    if parts.is_empty() {
        parts.push("0s".to_string());
    }

    parts.join(" ")
}

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
        let dashboard_url = services.config().webhooks.grey.dashboard_url.clone();

        // A stable key per monitor so a flapping entity reuses a single task rather than creating a
        // fresh notification each time it changes state.
        let unique_key = event.unique_key();
        // A distinct queue key for the deferred "recovered" confirmation so it can coexist with (and
        // be purged independently of) the task's active-state upserts, which share `unique_key`.
        let recovered_key = format!("{unique_key}/recovered");

        // The state machine reasons in terms of the event's own timestamp (Grey's clock) so that
        // durations stay internally consistent and webhook retries remain idempotent.
        let now = event.timestamp;
        let record = services
            .kv()
            .get::<GreyFailureRecord>(GREY_FAILURES_PARTITION, unique_key.clone())
            .await?;

        if event.state.healthy {
            // === RECOVERY ===
            // Cancel any still-pending alert. If the monitor recovered before its alert delay
            // elapsed, this suppresses the task entirely so a brief blip never surfaces.
            services
                .queue()
                .purge(TodoistUpsertTask::partition(), unique_key.clone())
                .await?;

            // Only walk the recovery path if a task actually surfaced. If the alert was still
            // pending (and we just purged it) there is nothing to recover, so we forget the incident.
            let task_exists = services
                .kv()
                .get::<TodoistUpsertTaskState>("todoist/task", unique_key.clone())
                .await?
                .is_some();

            if !task_exists {
                info!(
                    "Grey {} '{}' recovered before its alert surfaced; suppressing the task.",
                    event.entity.entity_type, event.entity.name
                );
                services
                    .kv()
                    .remove(GREY_FAILURES_PARTITION, unique_key.clone())
                    .await?;
                return Ok(());
            }

            // Total impact time runs from the *first* sign of failure in this incident to the
            // recovery, so a flapping incident is measured end-to-end rather than from its last
            // relapse.
            let first_unhealthy_at = record.as_ref().map(|r| r.first_unhealthy_at).unwrap_or(now);
            let impact = (now - first_unhealthy_at).max(chrono::Duration::zero());

            info!(
                "Grey {} '{}' recovered ({} -> {}); marking task as recovering (impact {}).",
                event.entity.entity_type,
                event.entity.name,
                event.state.previous,
                event.state.current,
                format_duration(impact),
            );

            // Immediately flip the existing task to "recovering" at a reduced priority.
            TodoistUpsertTask::dispatch(
                TodoistUpsertTaskPayload {
                    unique_key: unique_key.clone(),
                    title: event.recovering_title(dashboard_url.as_deref()),
                    description: Some(event.recovering_description()),
                    due: TodoistDueDate::DateTime(now),
                    priority: Some(RECOVERING_PRIORITY),
                    config: config.clone(),
                    ..Default::default()
                },
                Some(unique_key.clone().into()),
                services,
            )
            .await?;

            // Defer the "recovered" confirmation. A failure within the window purges this before it
            // fires; otherwise it stamps the task with the total impact time.
            TodoistUpsertTask::dispatch_delayed(
                TodoistUpsertTaskPayload {
                    unique_key: unique_key.clone(),
                    title: event.recovered_title(dashboard_url.as_deref(), impact),
                    description: Some(event.recovered_description(impact)),
                    due: TodoistDueDate::DateTime(now),
                    priority: Some(RECOVERING_PRIORITY),
                    config,
                    ..Default::default()
                },
                Some(recovered_key.into()),
                RECOVERY_WINDOW,
                services,
            )
            .await?;

            // Remember that we are recovering as of this event so a fresh failure within the window
            // re-escalates immediately rather than debouncing as a new incident.
            services
                .kv()
                .set(
                    GREY_FAILURES_PARTITION,
                    unique_key.clone(),
                    GreyFailureRecord {
                        first_unhealthy_at,
                        recovering_since: Some(now),
                    },
                )
                .await?;
        } else {
            // === UNHEALTHY ===
            let in_recovery_window = record
                .as_ref()
                .and_then(|r| r.recovering_since)
                .is_some_and(|since| now - since < RECOVERY_WINDOW);

            // Preserve the first-failure time across relapses inside the recovery window (and across
            // a continuing, not-yet-recovered failure) so impact is measured from the first sign of
            // failure. Only reset it when a genuinely new incident begins: there is no prior record,
            // or the previous incident fully recovered because its recovery window has elapsed.
            let first_unhealthy_at = match &record {
                Some(prev) if in_recovery_window || prev.recovering_since.is_none() => {
                    prev.first_unhealthy_at
                }
                _ => now,
            };

            if in_recovery_window {
                // The monitor failed again while we were showing it as recovering. Cancel the
                // pending "recovered" confirmation and re-escalate the task immediately, since an
                // operator is already watching it.
                info!(
                    "Grey {} '{}' failed again during recovery ({} -> {}); re-escalating immediately.",
                    event.entity.entity_type,
                    event.entity.name,
                    event.state.previous,
                    event.state.current
                );

                services
                    .queue()
                    .purge(TodoistUpsertTask::partition(), recovered_key)
                    .await?;

                TodoistUpsertTask::dispatch(
                    TodoistUpsertTaskPayload {
                        unique_key: unique_key.clone(),
                        title: event.task_title(dashboard_url.as_deref()),
                        description: Some(event.task_description()),
                        due: event
                            .state
                            .since
                            .map(TodoistDueDate::DateTime)
                            .unwrap_or(TodoistDueDate::DateTime(now)),
                        priority: Some(event.priority()),
                        config,
                        ..Default::default()
                    },
                    Some(unique_key.clone().into()),
                    services,
                )
                .await?;
            } else {
                // A fresh (or continuing) failure. Delay surfacing the task so a brief blip has time
                // to settle; a recovery received within the delay purges it before it is created.
                info!(
                    "Grey {} '{}' is unhealthy ({} -> {}); scheduling delayed alert in {}.",
                    event.entity.entity_type,
                    event.entity.name,
                    event.state.previous,
                    event.state.current,
                    format_duration(ALERT_DELAY),
                );

                TodoistUpsertTask::dispatch_delayed(
                    TodoistUpsertTaskPayload {
                        unique_key: unique_key.clone(),
                        title: event.task_title(dashboard_url.as_deref()),
                        description: Some(event.task_description()),
                        due: event
                            .state
                            .since
                            .map(TodoistDueDate::DateTime)
                            .unwrap_or(TodoistDueDate::DateTime(now)),
                        priority: Some(event.priority()),
                        config,
                        ..Default::default()
                    },
                    Some(unique_key.clone().into()),
                    ALERT_DELAY,
                    services,
                )
                .await?;
            }

            // Persist the incident's first-failure time and clear any recovering state.
            services
                .kv()
                .set(
                    GREY_FAILURES_PARTITION,
                    unique_key.clone(),
                    GreyFailureRecord {
                        first_unhealthy_at,
                        recovering_since: None,
                    },
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
    /// A stable per-monitor key (`grey/<type>/<name>`) used to correlate the Todoist task, the
    /// [`GREY_FAILURES_PARTITION`] state record, and the queue idempotency key for the task's
    /// active-state upserts.
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

    /// Builds a `**Grey**: <Entity> `<name>` <status>` title, linking back to the Grey status page
    /// when one is configured. The `status` clause describes the monitor's current situation, e.g.
    /// `is failing`, `is recovering`, or `has recovered after 12m`.
    fn title_with_status(&self, dashboard_url: Option<&str>, status: &str) -> String {
        let body = format!("{} `{}` {}", self.entity_label(), self.entity.name, status);

        match dashboard_url {
            Some(url) if !url.is_empty() => format!("[**Grey**]({url}): {body}"),
            _ => format!("**Grey**: {body}"),
        }
    }

    /// The Todoist task title for the monitor's current (unhealthy) state.
    fn task_title(&self, dashboard_url: Option<&str>) -> String {
        self.title_with_status(dashboard_url, &format!("is {}", self.state.current))
    }

    /// The title shown the moment a monitor reports healthy, while we wait out the recovery window.
    fn recovering_title(&self, dashboard_url: Option<&str>) -> String {
        self.title_with_status(dashboard_url, "is recovering")
    }

    /// The title stamped onto the task once a monitor has stayed healthy for the full recovery
    /// window, carrying the total impact time.
    fn recovered_title(&self, dashboard_url: Option<&str>, impact: chrono::Duration) -> String {
        self.title_with_status(
            dashboard_url,
            &format!("has recovered after {}", format_duration(impact)),
        )
    }

    /// The `- **Since:** … / - **Availability:** … / - **Tags:** …` context lines shared by every
    /// task description. Tags are sorted so the rendered description (and thus the upsert hash) is
    /// deterministic.
    fn detail_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();

        if let Some(since) = self.state.since {
            lines.push(format!("- **Since:** {}", since.to_rfc3339()));
        }

        if let Some(availability) = self.state.availability {
            lines.push(format!("- **Availability:** {availability:.2}%"));
        }

        if !self.entity.tags.is_empty() {
            let mut tags: Vec<_> = self.entity.tags.iter().collect();
            tags.sort();
            let rendered = tags
                .into_iter()
                .map(|(key, value)| format!("`{key}={value}`"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("- **Tags:** {rendered}"));
        }

        lines
    }

    /// The `_Event `…` (schema …)._` footer shared by every task description.
    fn event_footer(&self) -> String {
        format!("_Event `{}` (schema {})._", self.id, self.version)
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

        lines.extend(self.detail_lines());

        if let Some(detail) = self.failure_detail() {
            lines.push(String::new());
            lines.push(format!("**Latest detail:** {detail}"));
        }

        lines.push(String::new());
        lines.push(self.event_footer());

        lines.join("\n")
    }

    /// The description shown while a monitor is recovering, explaining that the task will confirm as
    /// recovered once the recovery window elapses without another failure.
    fn recovering_description(&self) -> String {
        let mut lines = vec![
            format!(
                "**{} `{}`** has reported healthy again and is **recovering**.",
                self.entity_label(),
                self.entity.name
            ),
            String::new(),
            format!(
                "It will be confirmed as recovered in {} if it stays healthy.",
                format_duration(RECOVERY_WINDOW)
            ),
            String::new(),
        ];

        lines.extend(self.detail_lines());

        lines.push(String::new());
        lines.push(self.event_footer());

        lines.join("\n")
    }

    /// The description stamped onto the task once a monitor has fully recovered, recording the total
    /// impact time of the incident.
    fn recovered_description(&self, impact: chrono::Duration) -> String {
        let mut lines = vec![
            format!(
                "**{} `{}`** has **recovered**.",
                self.entity_label(),
                self.entity.name
            ),
            String::new(),
            format!("- **Total impact time:** {}", format_duration(impact)),
        ];

        lines.extend(self.detail_lines());

        lines.push(String::new());
        lines.push(self.event_footer());

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
    use crate::db::PeekedMessage;
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

    /// Builds a `probe.state_changed` body with explicit event and `since` timestamps, so tests can
    /// drive the debounce state machine deterministically.
    fn probe_event_at(name: &str, healthy: bool, timestamp: &str, since: &str) -> String {
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
                "timestamp": "{timestamp}",
                "entity": {{ "type": "probe", "name": "{name}", "tags": {{ "service": "Web" }} }},
                "state": {{
                    "current": "{current}",
                    "previous": "{previous}",
                    "healthy": {healthy},
                    "was_healthy": {was_healthy},
                    "since": "{since}",
                    "availability": 98.7
                }},
                "probe": {{ "history": [{{ "pass": false, "message": "HTTP 503" }}] }}
            }}"#,
            was_healthy = !healthy
        )
    }

    /// A `probe.state_changed` body with the canonical fixed timestamps used by the parsing tests.
    fn probe_event(name: &str, healthy: bool) -> String {
        probe_event_at(
            name,
            healthy,
            "2026-06-19T12:00:00Z",
            "2026-06-19T11:59:30Z",
        )
    }

    fn webhook_event(body: String) -> WebhookEvent {
        WebhookEvent {
            body,
            query: String::new(),
            headers: HashMap::new(),
        }
    }

    /// Parses an RFC 3339 timestamp into a UTC instant. The explicit return type pins the otherwise
    /// ambiguous `FromStr` impl (chrono has one per timezone) so call sites stay terse.
    fn dt(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    /// Peeks every pending Todoist upsert enqueued by the handler.
    async fn peek_upserts<S: Services>(
        services: &S,
    ) -> Vec<PeekedMessage<TodoistUpsertTaskPayload>> {
        services
            .queue()
            .peek(TodoistUpsertTask::partition(), 16)
            .await
            .unwrap()
    }

    /// Fetches the persisted failure/recovery record for a monitor, if any.
    async fn failure_record<S: Services>(
        services: &S,
        unique_key: &str,
    ) -> Option<GreyFailureRecord> {
        services
            .kv()
            .get::<GreyFailureRecord>(GREY_FAILURES_PARTITION, unique_key.to_string())
            .await
            .unwrap()
    }

    /// Records an existing surfaced Todoist task so the recovery path treats the monitor as alerted.
    async fn seed_task<S: Services>(services: &S, unique_key: &str) {
        services
            .kv()
            .set(
                "todoist/task",
                unique_key.to_string(),
                TodoistUpsertTaskState {
                    id: "task-123".to_string(),
                    hash: "seed".to_string(),
                    title: Some("seed".to_string()),
                },
            )
            .await
            .unwrap();
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

    #[test]
    fn test_format_duration_is_compact() {
        use chrono::Duration;

        assert_eq!(format_duration(Duration::seconds(45)), "45s");
        assert_eq!(format_duration(Duration::minutes(12)), "12m");
        assert_eq!(format_duration(Duration::minutes(65)), "1h 5m");
        assert_eq!(format_duration(Duration::hours(2)), "2h");
        assert_eq!(format_duration(Duration::seconds(0)), "0s");
        // Negative spans are clamped, and sub-minute precision is dropped past an hour.
        assert_eq!(format_duration(Duration::seconds(-30)), "0s");
        assert_eq!(format_duration(Duration::seconds(3661)), "1h 1m");
    }

    #[test]
    fn test_recovering_and_recovered_titles() {
        let event: GreyWebhookEvent = serde_json::from_str(&probe_event("web.prod", true)).unwrap();

        assert_eq!(
            event.recovering_title(None),
            "**Grey**: Probe `web.prod` is recovering"
        );
        assert_eq!(
            event.recovered_title(
                Some("https://grey.example.com"),
                chrono::Duration::minutes(15)
            ),
            "[**Grey**](https://grey.example.com): Probe `web.prod` has recovered after 15m"
        );
    }

    #[test]
    fn test_recovered_description_reports_impact() {
        let event: GreyWebhookEvent = serde_json::from_str(&probe_event("web.prod", true)).unwrap();

        let description = event.recovered_description(chrono::Duration::minutes(15));
        assert!(description.contains("has **recovered**"));
        assert!(description.contains("- **Total impact time:** 15m"));
    }

    #[tokio::test]
    async fn test_unhealthy_schedules_delayed_alert() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let webhook = GreyWebhook;
        let unique_key = "grey/probe/web.prod";

        webhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                &webhook_event(probe_event("web.prod", false)),
            )
            .await
            .expect("unhealthy event should be handled");

        // No task is created immediately; a single delayed upsert is scheduled ~5 minutes out.
        let upserts = peek_upserts(&services).await;
        assert_eq!(
            upserts.len(),
            1,
            "exactly one delayed alert should be queued"
        );

        let alert = &upserts[0];
        assert_eq!(alert.key, unique_key);
        assert_eq!(alert.payload.priority, Some(4));
        assert!(alert.payload.title.contains("is failing"));
        assert!(
            alert.hidden_until > Utc::now() + chrono::Duration::minutes(4),
            "the alert should stay hidden for roughly the alert delay"
        );
        assert!(alert.hidden_until < Utc::now() + chrono::Duration::minutes(6));

        // The first-failure time is recorded, and we are not (yet) recovering.
        let record = failure_record(&services, unique_key)
            .await
            .expect("a failure record should be written");
        assert_eq!(record.first_unhealthy_at, dt("2026-06-19T12:00:00Z"));
        assert!(record.recovering_since.is_none());
    }

    #[tokio::test]
    async fn test_recovery_before_alert_surfaces_suppresses_it() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let webhook = GreyWebhook;
        let unique_key = "grey/probe/web.prod";

        // A fresh failure schedules a (still-pending) delayed alert.
        webhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                &webhook_event(probe_event("web.prod", false)),
            )
            .await
            .unwrap();
        assert_eq!(peek_upserts(&services).await.len(), 1);

        // Recovery arrives before the alert surfaced: the pending alert is purged and forgotten.
        webhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                &webhook_event(probe_event("web.prod", true)),
            )
            .await
            .unwrap();

        assert!(
            peek_upserts(&services).await.is_empty(),
            "the pending alert should be purged"
        );
        assert!(
            failure_record(&services, unique_key).await.is_none(),
            "the incident should be forgotten when it never surfaced"
        );
    }

    #[tokio::test]
    async fn test_recovery_marks_recovering_and_defers_recovered() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let webhook = GreyWebhook;
        let unique_key = "grey/probe/web.prod";

        // Pretend the alert already surfaced and the incident first went unhealthy 15 minutes ago.
        seed_task(&services, unique_key).await;
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                GreyFailureRecord {
                    first_unhealthy_at: dt("2026-06-19T11:45:00Z"),
                    recovering_since: None,
                },
            )
            .await
            .unwrap();

        // The healthy event is stamped at 12:00:00Z, so the impact is 15 minutes.
        webhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                &webhook_event(probe_event("web.prod", true)),
            )
            .await
            .unwrap();

        let upserts = peek_upserts(&services).await;
        assert_eq!(
            upserts.len(),
            2,
            "a recovering update and a deferred recovered update should be queued"
        );

        let recovering = upserts
            .iter()
            .find(|m| m.key == unique_key)
            .expect("an immediate recovering update");
        assert_eq!(recovering.payload.priority, Some(RECOVERING_PRIORITY));
        assert!(recovering.payload.title.contains("is recovering"));
        assert!(
            recovering.hidden_until <= Utc::now(),
            "the recovering update should fire immediately"
        );

        let recovered = upserts
            .iter()
            .find(|m| m.key == format!("{unique_key}/recovered"))
            .expect("a deferred recovered update");
        assert_eq!(recovered.payload.priority, Some(RECOVERING_PRIORITY));
        assert!(
            recovered.payload.title.contains("has recovered after 15m"),
            "the recovered update should carry the total impact time"
        );
        assert!(
            recovered.hidden_until > Utc::now() + chrono::Duration::minutes(55),
            "the recovered update should be deferred by roughly the recovery window"
        );

        // We now track that we are recovering as of the healthy event, retaining the first-failure
        // time so a later relapse still measures impact from the original outage.
        let record = failure_record(&services, unique_key)
            .await
            .expect("a recovery record");
        assert_eq!(record.recovering_since, Some(dt("2026-06-19T12:00:00Z")));
        assert_eq!(record.first_unhealthy_at, dt("2026-06-19T11:45:00Z"));
    }

    #[tokio::test]
    async fn test_refailure_during_recovery_reescalates_immediately() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let webhook = GreyWebhook;
        let unique_key = "grey/probe/web.prod";
        let recovered_key = format!("{unique_key}/recovered");

        // The monitor first failed at 11:30 and is currently recovering (since 11:45), with a
        // pending "recovered" confirmation queued for the end of the recovery window.
        seed_task(&services, unique_key).await;
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                GreyFailureRecord {
                    first_unhealthy_at: dt("2026-06-19T11:30:00Z"),
                    recovering_since: Some(dt("2026-06-19T11:45:00Z")),
                },
            )
            .await
            .unwrap();
        services
            .queue()
            .enqueue(
                TodoistUpsertTask::partition(),
                TodoistUpsertTaskPayload {
                    unique_key: unique_key.to_string(),
                    ..Default::default()
                },
                Some(recovered_key.clone().into()),
                Some(RECOVERY_WINDOW),
            )
            .await
            .unwrap();

        // A failure at 12:00 (15 minutes into the recovery window) re-escalates immediately.
        webhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                &webhook_event(probe_event("web.prod", false)),
            )
            .await
            .unwrap();

        let upserts = peek_upserts(&services).await;
        // The pending "recovered" confirmation is cancelled...
        assert!(
            upserts.iter().all(|m| m.key != recovered_key),
            "the deferred recovered confirmation should be purged"
        );
        // ...and the task is re-escalated to unhealthy immediately.
        let escalation = upserts
            .iter()
            .find(|m| m.key == unique_key)
            .expect("an immediate re-escalation");
        assert_eq!(escalation.payload.priority, Some(4));
        assert!(escalation.payload.title.contains("is failing"));
        assert!(
            escalation.hidden_until <= Utc::now(),
            "the re-escalation should fire immediately"
        );

        // The first-failure time is preserved (the relapse is part of the same incident) and the
        // recovery state is cleared.
        let record = failure_record(&services, unique_key)
            .await
            .expect("an updated record");
        assert_eq!(record.first_unhealthy_at, dt("2026-06-19T11:30:00Z"));
        assert!(
            record.recovering_since.is_none(),
            "recovery state should be cleared on re-failure"
        );
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
