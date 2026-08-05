//! Webhook handler for [Grey](https://github.com/SierraSoftworks/grey) state-change notifications.
//!
//! Grey delivers a JSON document whenever a probe or cron changes state. Rather than
//! surfacing every transition immediately, we classify each event through the reusable
//! [`crate::services::debounce`] detector — keyed by a stable `grey/<type>/<name>` key and
//! backed by the [`GREY_FAILURES_PARTITION`] table — and map its [`Detection`] onto Todoist actions
//! so a single task tells a coherent story from unhealthy, through recovering, to recovered:
//!
//! * [`Triggered`](Detection::Triggered) — the monitor is unhealthy. For a brand-new incident (the
//!   detection's `first_triggered_at` is the observation time) we schedule the operator's Todoist
//!   task [`GreyWebhookConfig::alert_delay`] into the future rather than creating it immediately; if
//!   the monitor recovers before the delay elapses the pending task is purged, so a brief blip never
//!   surfaces. When `first_triggered_at` is earlier the monitor has flapped back to unhealthy while
//!   recovering, so we re-escalate the task immediately (dated to the incident's first trigger)
//!   since an operator is already watching it.
//! * [`Recovering`](Detection::Recovering) — the monitor recovered after being triggered. If the
//!   recovery arrives before the alert delay has elapsed the operator's task never
//!   surfaced (its upsert is still pending), so we purge that pending alert and surface nothing — a
//!   brief blip never becomes a task. Otherwise the task has surfaced, so we immediately flip it to
//!   *recovering* at a reduced priority and defer a *recovered* update by the recovery delay,
//!   stamped with the total triggered duration. (Grey already debounces recovery internally for 5m,
//!   so a healthy report is a strong signal.) Any later trigger cancels that deferred update, so the
//!   recovery is only confirmed while it stays newer than the last trigger. Either way the debounce
//!   state is retained, so a relapse within the recovery window is still recognised as the same
//!   flapping incident and re-escalated immediately.

use std::fmt::Display;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    prelude::*,
    publishers::{
        TodoistCompleteTask, TodoistCompleteTaskPayload, TodoistDueDate, TodoistUpsertTask,
        TodoistUpsertTaskPayload,
    },
    services::debounce::{DebounceConfig, Debouncer, Detection},
};

/// The key/value partition holding each Grey monitor's debounce state
/// ([`crate::services::debounce::DebounceState`]), keyed by [`GreyWebhookEvent::unique_key`].
const GREY_FAILURES_PARTITION: &str = "grey/failures";

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

/// What one person asked us to do with the state changes their Grey reports.
///
/// There is deliberately no shared secret here, and no signature check. Grey's
/// HMAC used to be the only thing standing between this endpoint and anybody
/// who knew the installation's hostname, because the address itself was public
/// knowledge — one `/webhooks/grey` for the whole installation. A workflow now
/// has its own unguessable address which nothing else can be reached at and
/// which its owner can rotate, so the secret would be a second thing to
/// configure that proves nothing the address has not already proven.
#[derive(Clone, Serialize, Deserialize)]
pub struct GreyWebhookConfig {
    /// What to call this workflow, so it can be told apart from the others in a
    /// list of them.
    pub name: String,

    /// Optional base URL of the Grey status page, used to link the Todoist task back to Grey.
    #[serde(default)]
    pub dashboard_url: Option<String>,

    /// The amount of time to wait for a monitor to settle before surfacing the alert.
    #[serde(
        default = "default_alert_delay",
        with = "crate::serde_duration::minutes"
    )]
    pub alert_delay: chrono::Duration,

    /// The amount of time to wait after a monitor recovers before confirming the recovery.
    #[serde(
        default = "default_recovery_delay",
        with = "crate::serde_duration::minutes"
    )]
    pub recovery_delay: chrono::Duration,

    /// The minimum amount of impact required for a monitor's failure to stay in Todoist for later review.
    #[serde(
        default = "default_noise_duration",
        with = "crate::serde_duration::minutes"
    )]
    pub noise_duration: chrono::Duration,

    /// Filter applied to incoming events. The same fields Grey exposes to its own webhook filters
    /// are available here (`event`, `entity.*`, `state.*`).
    #[serde(default)]
    pub filter: crate::filter::Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: crate::publishers::TodoistTarget,
}

impl Display for GreyWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "grey/{}", self.name)
    }
}

impl Default for GreyWebhookConfig {
    /// Written out rather than derived, because a derived `Default` would leave
    /// every duration at zero — serde's `default = "…"` fallbacks only apply
    /// when deserializing, so the two paths would otherwise disagree about how
    /// long a monitor is given to settle.
    fn default() -> Self {
        Self {
            name: String::new(),
            dashboard_url: None,
            alert_delay: default_alert_delay(),
            recovery_delay: default_recovery_delay(),
            noise_duration: default_noise_duration(),
            filter: crate::filter::Filter::default(),
            todoist: default_todoist_config(),
        }
    }
}

fn default_alert_delay() -> chrono::Duration {
    chrono::Duration::minutes(5)
}

fn default_recovery_delay() -> chrono::Duration {
    chrono::Duration::hours(1)
}

fn default_noise_duration() -> chrono::Duration {
    chrono::Duration::minutes(5)
}

fn default_todoist_config() -> crate::publishers::TodoistTarget {
    crate::publishers::TodoistTarget {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct GreyWebhook;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Turns the state changes reported by
[Grey](https://github.com/SierraSoftworks/grey) into a single Todoist task per
monitor that tells a coherent story: raised when the monitor goes unhealthy,
updated when it recovers, and closed out afterwards if the incident turned out
not to matter. One monitor is one task, so a flapping probe does not produce a
task per flap.

What it deliberately does *not* do is tell you immediately. Monitors blip, and
an alert for every blip is an alert nobody reads, so the three waiting periods
below decide what actually reaches you.

## Getting the address

Save the workflow first. Its address is generated when it is created and shown
on the workflow afterwards; there is nothing to paste into Grey until then.

Then add a webhook to Grey's own configuration pointing at that address, and
restart or reload Grey so it picks the change up. Grey's `docs/guide/webhooks.md`
in [its repository](https://github.com/SierraSoftworks/grey) describes the
configuration block and the payload it sends.

There is no shared secret to configure. The address is unguessable and can be
rotated, which is what Grey's HMAC would have been proving.

**Status page** is optional and purely cosmetic: when set, each task links back
to it so you can see the wider picture without hunting for the address.

## The three waiting periods

These are what turn a stream of state changes into something worth reading.

**Wait before alerting** (default 5 minutes) is how long a monitor has to stay
unhealthy before you hear about it. The task is scheduled that far ahead rather
than created immediately, so a monitor that recovers inside the window has its
pending alert quietly withdrawn and never becomes a task at all.

**Wait before confirming recovery** (default 60 minutes) is how long a monitor
has to stay healthy before the incident is treated as over. A relapse inside
this window is recognised as the same incident and re-escalated immediately,
rather than starting the alert delay again — an operator is already watching
it, so making them wait five more minutes helps nobody.

**Keep incidents longer than** (default 5 minutes) decides what happens once a
monitor recovers. Incidents shorter than this are completed for you; anything
longer stays in Todoist for you to review, with the total impact time recorded
on it. Grey debounces recovery internally for five minutes, and that five
minutes is discounted before this comparison, so the duration you are setting a
threshold against is real impact rather than Grey's settling window.

## Choosing which monitors to act on

The filter runs against each state change and can match on `event`,
`entity.type`, `entity.name`, `state.current`, `state.previous`,
`state.healthy`, `state.was_healthy` and `state.availability`. A monitor's own
tags are available as `tags.<name>` (or `entity.tags.<name>`), which is usually
the most useful of the lot:

```
entity.type == "probe" && tags.environment == "production"
```

Leave it empty to act on every change Grey reports. Be careful narrowing it
after the fact: a filter that admits the unhealthy event but rejects the
recovery leaves a task open with nothing to close it.
"#;

crate::register_job!(GreyWebhook);
crate::register_workflow_type!(GreyWebhook);

impl crate::workflows::ConfigurableWorkflow for GreyWebhook {
    type ConfigType = GreyWebhookConfig;

    fn type_id() -> &'static str {
        "grey"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Grey".to_string(),
            description: "Raises a task when one of your Grey monitors goes unhealthy, and closes the story out when it recovers."
                .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                source: "grey".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Production monitors".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: dashboard_url),
                    "Status page",
                    FieldKind::Url {
                        placeholder: Some("https://grey.example.com/".into()),
                    },
                )
                .with_help(
                    "Optional. When set, each task links back here so you can see the wider picture without hunting for the address.",
                ),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: alert_delay),
                    "Wait before alerting (minutes)",
                    FieldKind::Number {
                        min: Some(0.0),
                        max: None,
                        step: Some(1.0),
                    },
                )
                .with_help(
                    "How long a monitor has to stay unhealthy before you hear about it. A blip that clears inside this window never becomes a task.",
                )
                .with_default(5),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: recovery_delay),
                    "Wait before confirming recovery (minutes)",
                    FieldKind::Number {
                        min: Some(0.0),
                        max: None,
                        step: Some(1.0),
                    },
                )
                .with_help(
                    "How long a monitor has to stay healthy before its task is tidied away. A relapse inside this window is treated as the same incident.",
                )
                .with_default(60),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: noise_duration),
                    "Keep incidents longer than (minutes)",
                    FieldKind::Number {
                        min: Some(0.0),
                        max: None,
                        step: Some(1.0),
                    },
                )
                .with_help(
                    "Incidents shorter than this are closed for you once they recover; anything longer stays in Todoist for you to review.",
                )
                .with_default(5),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec![
                            "event".into(),
                            "entity.type".into(),
                            "entity.name".into(),
                            "state.current".into(),
                            "state.previous".into(),
                            "state.healthy".into(),
                            "state.was_healthy".into(),
                            "state.availability".into(),
                        ],
                    },
                )
                .with_help(
                    "Only act on the state changes matching this, such as entity.type == \"probe\". A monitor's own tags are available as tags.<name>. Leave it empty to act on every change.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                GreyWebhookConfig,
                project = Some("Life"),
                section = Some("Tasks & Chores")
            ))
            .collect(),
        }
    }
}

impl Job for GreyWebhook {
    type JobType = crate::webhooks::WebhookDelivery;

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

        // Read now rather than carried in the delivery, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<GreyWebhookConfig>(services).await? else {
            return Ok(());
        };

        let event: GreyWebhookEvent = job.event.json()?;

        if !config.filter.matches(&event)? {
            info!(
                "Grey event for {} '{}' did not match filter; ignoring.",
                event.entity.entity_type, event.entity.name
            );
            return Ok(());
        }

        let todoist_config = config.todoist.clone();
        let dashboard_url = config.dashboard_url.clone();

        let now = event.timestamp;
        let unique_key = event.unique_key();

        let debouncer = Debouncer::new(
            services.kv(),
            GREY_FAILURES_PARTITION,
            DebounceConfig {
                window: config.recovery_delay,
            },
        );

        let state = if event.state.healthy {
            debouncer.on_recovered(&unique_key, now).await?
        } else {
            Some(debouncer.on_triggered(&unique_key, now).await?)
        };

        match state {
            None => {
                // We recovered without a record of a failure, so there's nothing to do.
                info!(
                    "Grey {} '{}' sent a recovery event without a prior failure record, ignoring.",
                    event.entity.entity_type, event.entity.name
                );
            }
            Some(Detection::Triggered { first_triggered_at }) => {
                info!(
                    "Grey {} '{}' sent an alert.",
                    event.entity.entity_type, event.entity.name,
                );

                services
                    .queue()
                    .purge(TodoistCompleteTask::partition(), unique_key.clone())
                    .await?;

                TodoistUpsertTask::dispatch_delayed(
                    TodoistUpsertTaskPayload {
                        unique_key: unique_key.clone(),
                        title: event.task_title(dashboard_url.as_deref()),
                        description: Some(event.task_description()),
                        due: TodoistDueDate::DateTime(first_triggered_at),
                        priority: Some(event.priority()),
                        config: todoist_config,
                        ..Default::default()
                    },
                    Some(unique_key.clone().into()),
                    ((first_triggered_at + config.alert_delay) - now).max(chrono::Duration::zero()),
                    services,
                )
                .await?;
            }
            Some(Detection::Recovering { triggered_for }) => {
                // If the monitor returned to healthy before the alert debounce window elapsed, the
                // operator's task never surfaced — its delayed upsert is still sitting in the queue —
                // so cancel that pending alert and don't surface anything. A brief blip that clears
                // within the alert window must never reach Todoist (previously these produced spurious
                // "recovered after 0s" tasks). The debounce state is deliberately left in place so a
                // relapse within the recovery window is still recognised as the same flapping incident
                // and re-escalated immediately.
                if triggered_for < config.alert_delay {
                    info!(
                        "Grey {} '{}' recovered ({} -> {}) after only {}, before the {} alert window elapsed; purging the pending alert without surfacing a task.",
                        event.entity.entity_type,
                        event.entity.name,
                        event.state.previous,
                        event.state.current,
                        format_duration(triggered_for),
                        format_duration(config.alert_delay),
                    );

                    services
                        .queue()
                        .purge(TodoistUpsertTask::partition(), unique_key.clone())
                        .await?;

                    return Ok(());
                }

                info!(
                    "Grey {} '{}' recovered ({} -> {}); triggered for {}, confirming in {}.",
                    event.entity.entity_type,
                    event.entity.name,
                    event.state.previous,
                    event.state.current,
                    format_duration(triggered_for),
                    format_duration(config.recovery_delay),
                );

                // Grey internally has a 5m settling window before it sends a resolved event, so let's adjust for that
                let true_impact_duration = triggered_for - chrono::Duration::minutes(5);

                TodoistUpsertTask::dispatch(
                    TodoistUpsertTaskPayload {
                        unique_key: unique_key.clone(),
                        title: event
                            .recovered_title(dashboard_url.as_deref(), true_impact_duration),
                        description: Some(event.recovered_description(true_impact_duration)),
                        due: TodoistDueDate::DateTime(now),
                        priority: Some(2),
                        config: todoist_config.clone(),
                        ..Default::default()
                    },
                    Some(unique_key.clone().into()),
                    services,
                )
                .await?;

                if true_impact_duration < config.noise_duration {
                    TodoistCompleteTask::dispatch_delayed(
                        TodoistCompleteTaskPayload {
                            unique_key: unique_key.clone(),
                            config: todoist_config,
                        },
                        Some(unique_key.into()),
                        config.recovery_delay,
                        services,
                    )
                    .await?;
                }
            }
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

    /// The title stamped onto the task once a monitor has stayed healthy for the full recovery
    /// window, carrying the total impact time.
    fn recovered_title(&self, dashboard_url: Option<&str>, impact: chrono::Duration) -> String {
        self.title_with_status(
            dashboard_url,
            &format!("recovered after {}", format_duration(impact)),
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
    use std::collections::HashMap;

    use crate::db::PeekedMessage;
    use crate::publishers::TodoistUpsertTaskState;
    use crate::services::debounce::DebounceState;
    use crate::webhooks::{WebhookDelivery, WebhookEvent};
    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    use super::*;

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

    /// The timings a real deployment uses, stated explicitly so the debounce behaviour is
    /// exercised rather than collapsing onto whatever the defaults happen to be.
    fn config() -> serde_json::Value {
        serde_json::json!({
            "name": "Production monitors",
            "alert_delay": 5,
            "recovery_delay": 60,
            "noise_duration": 5,
        })
    }

    async fn mock_services() -> crate::services::ServicesContainer<crate::db::TenantDb> {
        crate::services::ServicesContainer::new_mock()
            .await
            .unwrap()
    }

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "grey".into(),
                config,
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id
    }

    fn delivery(workflow: automate_api::WorkflowId, body: String) -> WebhookDelivery {
        WebhookDelivery {
            workflow,
            event: WebhookEvent {
                body,
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
        GreyWebhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                delivery,
            )
            .await
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

    /// Peeks every pending Todoist cleanup (complete-task) enqueued by the handler.
    async fn peek_completes<S: Services>(
        services: &S,
    ) -> Vec<PeekedMessage<TodoistCompleteTaskPayload>> {
        services
            .queue()
            .peek(TodoistCompleteTask::partition(), 16)
            .await
            .unwrap()
    }

    /// Fetches the persisted debounce state for a monitor, if any.
    async fn failure_record<S: Services>(services: &S, unique_key: &str) -> Option<DebounceState> {
        services
            .kv()
            .get::<DebounceState>(GREY_FAILURES_PARTITION, unique_key.to_string())
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
    fn test_recovered_titles() {
        let event: GreyWebhookEvent = serde_json::from_str(&probe_event("web.prod", true)).unwrap();

        assert_eq!(
            event.recovered_title(
                Some("https://grey.example.com"),
                chrono::Duration::minutes(15)
            ),
            "[**Grey**](https://grey.example.com): Probe `web.prod` recovered after 15m"
        );
    }

    #[test]
    fn test_recovered_description_reports_impact() {
        let event: GreyWebhookEvent = serde_json::from_str(&probe_event("web.prod", true)).unwrap();

        let description = event.recovered_description(chrono::Duration::minutes(15));
        assert!(description.contains("has **recovered**"));
        assert!(description.contains("- **Total impact time:** 15m"));
    }

    #[test]
    fn a_waiting_period_is_written_down_as_a_number_of_minutes() {
        // The form collects minutes, so a stored configuration has to hold them
        // as a plain number rather than as chrono's (seconds, nanos) pair —
        // otherwise nothing anybody could type into the field would load.
        let config: GreyWebhookConfig = serde_json::from_value(serde_json::json!({
            "name": "Production monitors",
            "alert_delay": 15,
        }))
        .expect("a configuration in minutes should load");

        assert_eq!(config.alert_delay, chrono::Duration::minutes(15));
        assert_eq!(
            config.recovery_delay,
            chrono::Duration::hours(1),
            "an omitted waiting period should fall back to its default rather than to zero",
        );

        let round_tripped = serde_json::to_value(&config).unwrap();
        assert_eq!(round_tripped["alert_delay"], 15);
    }

    #[test]
    fn a_negative_waiting_period_is_refused_by_name() {
        // "Wait minus five minutes" is not a thing we could do, and silently
        // treating it as zero would alert instantly on a monitor somebody
        // thought they had told us to be patient about.
        let Err(err) = serde_json::from_value::<GreyWebhookConfig>(serde_json::json!({
            "name": "Production monitors",
            "alert_delay": -5,
        })) else {
            panic!("a negative waiting period should not load");
        };

        assert!(format!("{err}").contains("negative"), "{err}");
    }

    #[test]
    fn a_stored_configuration_names_the_workflow_it_describes() {
        let workflow = crate::workflows::lookup("grey").expect("the type is registered");

        assert_eq!(
            workflow
                .describe(&serde_json::json!({ "name": "Production monitors" }))
                .unwrap(),
            "Production monitors",
        );
    }

    #[tokio::test]
    async fn test_unhealthy_schedules_delayed_alert() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", false)),
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

        // A brand-new failure never schedules a cleanup; there is nothing to tidy up yet.
        assert!(
            peek_completes(&services).await.is_empty(),
            "no cleanup should be queued for a fresh failure"
        );

        // The first- and last-failure times are recorded, and we are not (yet) recovering.
        let record = failure_record(&services, unique_key)
            .await
            .expect("a failure record should be written");
        assert_eq!(record.first_triggered_at, dt("2026-06-19T12:00:00Z"));
        assert_eq!(record.last_triggered_at, dt("2026-06-19T12:00:00Z"));
        assert!(record.recovering_since.is_none());
    }

    #[tokio::test]
    async fn test_recovery_without_prior_failure_is_ignored() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // A healthy event with no recorded incident is a no-op: nothing to update, nothing to clean.
        run(
            &services,
            &delivery(workflow, probe_event("web.prod", true)),
        )
        .await
        .unwrap();

        assert!(
            peek_upserts(&services).await.is_empty(),
            "no task should be touched without a prior failure"
        );
        assert!(
            peek_completes(&services).await.is_empty(),
            "no cleanup should be queued without a prior failure"
        );
        assert!(
            failure_record(&services, unique_key).await.is_none(),
            "no debounce state should be created for a stray recovery"
        );
    }

    #[tokio::test]
    async fn test_recovery_updates_task_and_schedules_cleanup_when_noise() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // The alert already surfaced (a task exists) and a delayed alert is still queued from when
        // the incident first fired. The incident first went unhealthy at 11:53, so by the 12:00
        // recovery it was triggered for 7m — a true impact of ~2m once Grey's 5m settling window is
        // discounted, which is below the 5m noise threshold.
        seed_task(&services, unique_key).await;
        services
            .queue()
            .enqueue(
                TodoistUpsertTask::partition(),
                TodoistUpsertTaskPayload {
                    unique_key: unique_key.to_string(),
                    ..Default::default()
                },
                Some(unique_key.to_string().into()),
                Some(chrono::Duration::minutes(5)),
            )
            .await
            .unwrap();
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T11:53:00Z"),
                    last_triggered_at: dt("2026-06-19T11:55:00Z"),
                    recovering_since: None,
                },
            )
            .await
            .unwrap();

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", true)),
        )
        .await
        .unwrap();

        // The pending alert is replaced in place by an immediate recovered update carrying the true
        // impact time (~2m).
        let upserts = peek_upserts(&services).await;
        assert_eq!(
            upserts.len(),
            1,
            "the recovered update should replace the pending alert on the same key"
        );
        let recovered = &upserts[0];
        assert_eq!(recovered.key, unique_key);
        assert_eq!(recovered.payload.priority, Some(2));
        assert!(
            recovered.payload.title.contains("recovered after 2m"),
            "the recovered update should carry the true impact time, got {:?}",
            recovered.payload.title
        );
        assert!(
            recovered.hidden_until <= Utc::now(),
            "the recovered update should fire immediately"
        );

        // Because it was just noise, a cleanup is deferred by the recovery window (~1h) to remove the
        // task once we are confident it was not a flap.
        let completes = peek_completes(&services).await;
        assert_eq!(completes.len(), 1, "a delayed cleanup should be queued");
        assert_eq!(completes[0].key, unique_key);
        assert!(
            completes[0].hidden_until > Utc::now() + chrono::Duration::minutes(55),
            "the cleanup should be deferred by roughly the recovery window"
        );

        // We now track that we are recovering as of the healthy event, retaining the first-failure
        // time so a later relapse still measures impact from the original outage.
        let record = failure_record(&services, unique_key)
            .await
            .expect("a recovery record");
        assert_eq!(record.recovering_since, Some(dt("2026-06-19T12:00:00Z")));
        assert_eq!(record.first_triggered_at, dt("2026-06-19T11:53:00Z"));
    }

    #[tokio::test]
    async fn test_recovery_updates_task_without_cleanup_when_impactful() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // The incident first went unhealthy at 11:45, so by the 12:00 recovery it was triggered for
        // 15m — a true impact of 10m once Grey's 5m settling window is discounted, which exceeds the
        // 5m noise threshold and therefore stays in Todoist for review.
        seed_task(&services, unique_key).await;
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T11:45:00Z"),
                    last_triggered_at: dt("2026-06-19T11:50:00Z"),
                    recovering_since: None,
                },
            )
            .await
            .unwrap();

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", true)),
        )
        .await
        .unwrap();

        // An immediate recovered update carries the true impact time (10m)...
        let upserts = peek_upserts(&services).await;
        assert_eq!(upserts.len(), 1, "one recovered update should be queued");
        let recovered = &upserts[0];
        assert_eq!(recovered.key, unique_key);
        assert_eq!(recovered.payload.priority, Some(2));
        assert!(
            recovered.payload.title.contains("recovered after 10m"),
            "the recovered update should carry the true impact time, got {:?}",
            recovered.payload.title
        );
        assert!(
            recovered.hidden_until <= Utc::now(),
            "the recovered update should fire immediately"
        );

        // ...but because the impact was above the noise threshold, no cleanup is scheduled: the task
        // stays for the operator to review.
        assert!(
            peek_completes(&services).await.is_empty(),
            "an impactful incident should not schedule a cleanup"
        );

        let record = failure_record(&services, unique_key)
            .await
            .expect("a recovery record");
        assert_eq!(record.recovering_since, Some(dt("2026-06-19T12:00:00Z")));
        assert_eq!(record.first_triggered_at, dt("2026-06-19T11:45:00Z"));
    }

    #[tokio::test]
    async fn test_recovery_before_alert_surfaces_purges_pending_alert() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // The monitor first went unhealthy at 11:58 and its alert is still sitting in the queue
        // (it only fires at 12:03, once the 5m alert window elapses). The 12:00 recovery therefore
        // lands inside the alert window — only 2m of impact — before the operator was ever notified.
        services
            .queue()
            .enqueue(
                TodoistUpsertTask::partition(),
                TodoistUpsertTaskPayload {
                    unique_key: unique_key.to_string(),
                    ..Default::default()
                },
                Some(unique_key.to_string().into()),
                Some(chrono::Duration::minutes(3)),
            )
            .await
            .unwrap();
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T11:58:00Z"),
                    last_triggered_at: dt("2026-06-19T11:58:00Z"),
                    recovering_since: None,
                },
            )
            .await
            .unwrap();

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", true)),
        )
        .await
        .unwrap();

        // The pending alert is purged and nothing is surfaced: a blip that clears before the alert
        // window elapses never reaches Todoist (previously this produced a "recovered after 0s" task).
        assert!(
            peek_upserts(&services).await.is_empty(),
            "the pending alert should be purged and no recovered task surfaced"
        );
        assert!(
            peek_completes(&services).await.is_empty(),
            "no cleanup should be queued for an unsurfaced blip"
        );

        // Crucially the debounce state is retained (now marked recovering, first-failure time
        // preserved) so we still know when the probe last failed and can treat a relapse as flapping.
        let record = failure_record(&services, unique_key)
            .await
            .expect("the debounce state should be retained when the alert never surfaced");
        assert_eq!(record.first_triggered_at, dt("2026-06-19T11:58:00Z"));
        assert_eq!(record.recovering_since, Some(dt("2026-06-19T12:00:00Z")));

        // A relapse within the recovery window is therefore recognised as the same incident and
        // re-escalated immediately (dated to the original 11:58 first failure), not debounced afresh.
        // (12:05 is past the original 12:03 alert deadline, so the escalation fires immediately.)
        run(
            &services,
            &delivery(
                workflow,
                probe_event_at(
                    "web.prod",
                    false,
                    "2026-06-19T12:05:00Z",
                    "2026-06-19T12:05:00Z",
                ),
            ),
        )
        .await
        .unwrap();

        let upserts = peek_upserts(&services).await;
        let escalation = upserts
            .iter()
            .find(|m| m.key == unique_key)
            .expect("the relapse should re-escalate the alert");
        assert!(
            escalation.hidden_until <= Utc::now(),
            "a relapse of an ongoing incident should re-escalate immediately"
        );
        assert!(escalation.payload.title.contains("is failing"));
    }

    #[tokio::test]
    async fn test_refailure_after_settling_reescalates_and_cancels_cleanup() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // The monitor first failed at 11:30, last reported unhealthy at 11:40, and is currently
        // recovering (since 11:45), with a pending noise cleanup queued to remove the task.
        seed_task(&services, unique_key).await;
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T11:30:00Z"),
                    last_triggered_at: dt("2026-06-19T11:40:00Z"),
                    recovering_since: Some(dt("2026-06-19T11:45:00Z")),
                },
            )
            .await
            .unwrap();
        services
            .queue()
            .enqueue(
                TodoistCompleteTask::partition(),
                TodoistCompleteTaskPayload {
                    unique_key: unique_key.to_string(),
                    ..Default::default()
                },
                Some(unique_key.to_string().into()),
                Some(chrono::Duration::minutes(60)),
            )
            .await
            .unwrap();

        // A failure at 12:00 — 20 minutes after the last unhealthy report, so inside the recovery
        // window — and well beyond the original 5m settling time, so it re-escalates immediately.
        run(
            &services,
            &delivery(workflow, probe_event("web.prod", false)),
        )
        .await
        .unwrap();

        // The pending cleanup is cancelled so the task cannot be removed while the monitor is down...
        assert!(
            peek_completes(&services).await.is_empty(),
            "the pending cleanup should be purged on re-failure"
        );
        // ...and the task is re-escalated to unhealthy immediately.
        let upserts = peek_upserts(&services).await;
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

        // The first-failure time is preserved (the relapse is part of the same incident), the last
        // failure advances to now, and the recovery state is cleared.
        let record = failure_record(&services, unique_key)
            .await
            .expect("an updated record");
        assert_eq!(record.first_triggered_at, dt("2026-06-19T11:30:00Z"));
        assert_eq!(record.last_triggered_at, dt("2026-06-19T12:00:00Z"));
        assert!(
            record.recovering_since.is_none(),
            "recovery state should be cleared on re-failure"
        );
    }

    #[tokio::test]
    async fn test_new_incident_failure_purges_stale_cleanup() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // A previous incident recovered at 10:45 (last failure 10:30) and left a pending noise
        // cleanup queued.
        seed_task(&services, unique_key).await;
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T10:00:00Z"),
                    last_triggered_at: dt("2026-06-19T10:30:00Z"),
                    recovering_since: Some(dt("2026-06-19T10:45:00Z")),
                },
            )
            .await
            .unwrap();
        services
            .queue()
            .enqueue(
                TodoistCompleteTask::partition(),
                TodoistCompleteTaskPayload {
                    unique_key: unique_key.to_string(),
                    ..Default::default()
                },
                Some(unique_key.to_string().into()),
                Some(chrono::Duration::minutes(60)),
            )
            .await
            .unwrap();

        // A failure at 12:00 — 90 minutes after the last failure, so a brand-new incident, not a
        // relapse — must still cancel the stale cleanup so it can never remove the task while the
        // monitor is unhealthy.
        run(
            &services,
            &delivery(workflow, probe_event("web.prod", false)),
        )
        .await
        .unwrap();

        assert!(
            peek_completes(&services).await.is_empty(),
            "the stale cleanup should be purged even for a new incident"
        );
        // It is a new incident: the alert is delayed (not an immediate escalation) and the
        // first-failure time is reset.
        let upserts = peek_upserts(&services).await;
        let alert = upserts
            .iter()
            .find(|m| m.key == unique_key)
            .expect("a delayed alert");
        assert!(
            alert.hidden_until > Utc::now() + chrono::Duration::minutes(4),
            "a new incident should be debounced, not escalated immediately"
        );
        assert_eq!(
            failure_record(&services, unique_key)
                .await
                .unwrap()
                .first_triggered_at,
            dt("2026-06-19T12:00:00Z")
        );
    }

    #[tokio::test]
    async fn an_event_the_filter_rejects_is_ignored() {
        // The filter is the only thing between a busy tailnet of monitors and a
        // task for every one of them, so a state change it does not match has to
        // leave no trace at all — not even debounce state.
        let services = mock_services().await;
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Crons only",
                "filter": r#"entity.type == "cron""#,
            }),
        )
        .await;

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", false)),
        )
        .await
        .unwrap();

        assert!(peek_upserts(&services).await.is_empty());
        assert!(
            failure_record(&services, "grey/probe/web.prod")
                .await
                .is_none(),
        );
    }

    #[tokio::test]
    async fn test_grey_webhook_invalid_json() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;

        let result = run(
            &services,
            &delivery(workflow, r#"{"invalid json"#.to_string()),
        )
        .await;

        assert!(result.is_err(), "Webhook should reject invalid JSON");
    }
}
