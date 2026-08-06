//! The records demo mode starts from.
//!
//! Everything here is written to be *representative* rather than exhaustive:
//! enough variety that every state a page can be in shows up somewhere (a
//! connection that needs reconnecting, a paused workflow, an expired cache
//! entry), without pretending to be a plausible production database. The
//! exhaustive per-control coverage lives in the control gallery instead, which
//! is the thing built for it.

use automate_api::{
    AdminUser, AuditCategory, AuditOutcome, AuditRecord, Connection, ConnectionId, ConnectionKind,
    ConnectionStatus, ConnectionSummary, FieldDescriptor, FieldKind, IntegrationInfo,
    KeyValueEntry, OptionItem, QueueMessage, QueueStatus, RunOutcome, RunReport, RunState,
    TenantId, Workflow, WorkflowId, WorkflowTrigger, WorkflowTypeDescriptor,
};
use chrono::{Duration, Utc};
use serde_json::json;

/// A sample signed-in user for demo mode.
pub fn admin_user() -> AdminUser {
    AdminUser {
        email: Some("demo@example.com".to_string()),
        ..AdminUser::new("Demo User")
    }
}

/// Sample key-value entries spanning a couple of partitions. One payload
/// deliberately contains HTML to demonstrate that payloads are rendered as
/// escaped text.
pub fn kv_entries() -> Vec<KeyValueEntry> {
    vec![
        KeyValueEntry::new(
            "github_notifications",
            "PR-1042",
            json!({
                "title": "Refactor the web UI into separate crates",
                "url": "https://github.com/SierraSoftworks/automate/pull/1042",
                "unread": true
            }),
        ),
        KeyValueEntry::new(
            "github_notifications",
            "ISSUE-1043",
            json!({
                "title": "<script>alert('xss')</script> rendered safely",
                "url": "https://github.com/SierraSoftworks/automate/issues/1043",
                "unread": false
            }),
        ),
        KeyValueEntry::new(
            "rss_state",
            "https://example.com/feed.xml",
            json!({
                "last_seen": "2024-05-01T12:00:00Z",
                "etag": "\"a1b2c3\""
            }),
        ),
        // Cached entries wrap their payload in a `value`/`expires_at` envelope.
        // This one is still fresh (expires in the future).
        KeyValueEntry::new(
            "alphavantage/quote",
            "MSFT",
            json!({
                "value": 423.85,
                "expires_at": (Utc::now() + Duration::hours(6)).to_rfc3339()
            }),
        ),
        // An expired cache entry (its `expires_at` is in the past).
        KeyValueEntry::new(
            "oidc:discovery",
            "https://accounts.google.com",
            json!({
                "value": {
                    "issuer": "https://accounts.google.com",
                    "authorization_endpoint": "https://accounts.google.com/o/oauth2/v2/auth",
                    "token_endpoint": "https://oauth2.googleapis.com/token",
                    "jwks_uri": "https://www.googleapis.com/oauth2/v3/certs"
                },
                "expires_at": (Utc::now() - Duration::minutes(30)).to_rfc3339()
            }),
        ),
    ]
}

/// Sample queued messages covering the three message states.
pub fn queue_messages() -> Vec<QueueMessage> {
    let now = Utc::now();
    vec![
        // Pending: enqueued a while ago and available now (no hidden span, so the
        // timeline collapses to the pending state node with a notification dot).
        QueueMessage {
            partition: "todoist_create".to_string(),
            key: "task-001".to_string(),
            payload: json!({ "content": "Review the deployment runbook", "project": "Software" }),
            status: QueueStatus::Pending,
            scheduled_at: now - Duration::minutes(15),
            hidden_until: None,
            traceparent: None,
        },
        // Delayed: a short hidden span with the "now" marker roughly a third of
        // the way along.
        QueueMessage {
            partition: "github_notifications".to_string(),
            key: "notif-7781".to_string(),
            payload: json!({ "action": "archive", "thread": 7781 }),
            status: QueueStatus::Delayed,
            scheduled_at: now - Duration::seconds(90),
            hidden_until: Some(now + Duration::minutes(4)),
            traceparent: Some(
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
            ),
        },
        // Delayed further out: the marker sits near the start of a long span.
        QueueMessage {
            partition: "github_notifications".to_string(),
            key: "notif-9920".to_string(),
            payload: json!({ "action": "snooze", "thread": 9920 }),
            status: QueueStatus::Delayed,
            scheduled_at: now - Duration::minutes(5),
            hidden_until: Some(now + Duration::hours(2)),
            traceparent: None,
        },
        // Reserved: actively processing, so the timeline shows the spinning retry
        // glyph and no "now" marker.
        QueueMessage {
            partition: "spotify_add_to_playlist".to_string(),
            key: "track-55".to_string(),
            payload: json!({ "track": "spotify:track:55", "playlist": "Liked 2024" }),
            status: QueueStatus::Reserved,
            scheduled_at: now - Duration::seconds(20),
            hidden_until: Some(now + Duration::seconds(40)),
            traceparent: None,
        },
    ]
}

/// Sample linked accounts, one per credential shape and one per status, so the
/// connections page shows every badge it can draw.
pub fn service_connections() -> Vec<ConnectionSummary> {
    let now = Utc::now();
    vec![
        ConnectionSummary {
            id: ConnectionId::from_entropy(1),
            provider: "todoist".to_string(),
            kind: ConnectionKind::ApiKey,
            name: "Personal".to_string(),
            account: Some("demo@example.com".to_string()),
            status: ConnectionStatus::Ok,
            expires_at: None,
            metadata: Default::default(),
            created_at: now - Duration::days(120),
            updated_at: now - Duration::days(120),
        },
        ConnectionSummary {
            id: ConnectionId::from_entropy(2),
            provider: "github".to_string(),
            kind: ConnectionKind::GitHubApp,
            name: "SierraSoftworks".to_string(),
            account: Some("SierraSoftworks".to_string()),
            status: ConnectionStatus::Ok,
            expires_at: None,
            metadata: serde_json::Map::from_iter([(
                "account_type".to_string(),
                json!("Organization"),
            )]),
            created_at: now - Duration::days(30),
            updated_at: now - Duration::hours(2),
        },
        ConnectionSummary {
            id: ConnectionId::from_entropy(3),
            provider: "spotify".to_string(),
            kind: ConnectionKind::OAuth2,
            name: "Spotify".to_string(),
            account: Some("demo".to_string()),
            status: ConnectionStatus::NeedsReauthorization,
            expires_at: Some(now - Duration::hours(3)),
            metadata: Default::default(),
            created_at: now - Duration::days(200),
            updated_at: now - Duration::hours(3),
        },
        ConnectionSummary {
            id: ConnectionId::from_entropy(4),
            provider: "ynab".to_string(),
            kind: ConnectionKind::ApiKey,
            name: "Budget".to_string(),
            account: None,
            status: ConnectionStatus::Error,
            expires_at: None,
            metadata: Default::default(),
            created_at: now - Duration::days(14),
            updated_at: now - Duration::minutes(45),
        },
    ]
}

/// The workflow types demo mode offers, between them exercising every trigger
/// and most of the field kinds a real descriptor can use.
pub fn workflow_types() -> Vec<WorkflowTypeDescriptor> {
    vec![
        WorkflowTypeDescriptor {
            id: "rss".to_string(),
            name: "RSS feed".to_string(),
            description: "Turn new items in a feed into Todoist tasks.".to_string(),
            documentation: RSS_DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "0 */6 * * *".to_string(),
            },
            fields: vec![
                FieldDescriptor::new(
                    "feed.url",
                    "Feed address",
                    FieldKind::Url {
                        placeholder: Some("https://example.com/feed.xml".to_string()),
                    },
                )
                .with_help("The RSS or Atom address to poll.")
                .required(),
                FieldDescriptor::new(
                    "filter",
                    "Only include items matching",
                    FieldKind::Filter {
                        fields: vec![
                            "title".to_string(),
                            "link".to_string(),
                            "summary".to_string(),
                            "author".to_string(),
                        ],
                    },
                )
                .with_help("Leave empty to take every item in the feed."),
                FieldDescriptor::new(
                    "todoist.connection",
                    "Todoist account",
                    FieldKind::Connection {
                        provider: "todoist".to_string(),
                        connection_kind: None,
                    },
                )
                .required(),
                FieldDescriptor::new(
                    "todoist.project",
                    "Project",
                    FieldKind::Options {
                        source: "projects".to_string(),
                        depends_on: "todoist.connection".to_string(),
                        parent: None,
                    },
                )
                .required(),
                FieldDescriptor::new(
                    "todoist.section",
                    "Section",
                    FieldKind::Options {
                        source: "sections".to_string(),
                        depends_on: "todoist.connection".to_string(),
                        parent: Some("todoist.project".to_string()),
                    },
                )
                .with_help("Optional. Narrowed to the sections of the chosen project."),
                FieldDescriptor::new(
                    "todoist.priority",
                    "Priority",
                    FieldKind::Number {
                        min: Some(1.0),
                        max: Some(4.0),
                        step: Some(1.0),
                    },
                )
                .with_default(1),
                FieldDescriptor::new("include_summary", "Include the summary", FieldKind::Boolean)
                    .with_help("Adds the item's summary to the task description.")
                    .with_default(true),
            ],
        },
        WorkflowTypeDescriptor {
            id: "github_notifications".to_string(),
            name: "GitHub notifications".to_string(),
            description: "Triage your GitHub inbox into tasks you can act on.".to_string(),
            documentation: String::new(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "*/15 * * * *".to_string(),
            },
            fields: vec![
                FieldDescriptor::new(
                    "github.connection",
                    "GitHub account",
                    FieldKind::Connection {
                        provider: "github".to_string(),
                        connection_kind: Some(ConnectionKind::GitHubApp),
                    },
                )
                .required(),
                FieldDescriptor::new(
                    "reason",
                    "Only notifications because I am",
                    FieldKind::Select {
                        options: vec![
                            OptionItem::new("any", "Anything at all").as_default(),
                            OptionItem::new("participating", "Participating"),
                            OptionItem::new("mention", "Mentioned"),
                            OptionItem::new("review_requested", "Asked to review"),
                        ],
                    },
                )
                .with_default("any"),
                FieldDescriptor::new(
                    "todoist.connection",
                    "Todoist account",
                    FieldKind::Connection {
                        provider: "todoist".to_string(),
                        connection_kind: None,
                    },
                )
                .required(),
                FieldDescriptor::new(
                    "todoist.project",
                    "Project",
                    FieldKind::Options {
                        source: "projects".to_string(),
                        depends_on: "todoist.connection".to_string(),
                        parent: None,
                    },
                )
                .required(),
            ],
        },
        WorkflowTypeDescriptor {
            id: "github_webhook".to_string(),
            name: "GitHub webhook".to_string(),
            description: "React to deliveries GitHub sends to your own address.".to_string(),
            documentation: WEBHOOK_DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                source: "github".to_string(),
            },
            fields: vec![
                FieldDescriptor::new(
                    "event",
                    "Event",
                    FieldKind::Select {
                        options: vec![
                            OptionItem::new("push", "Push"),
                            OptionItem::new("pull_request", "Pull request").as_default(),
                            OptionItem::new("release", "Release"),
                        ],
                    },
                )
                .required(),
                FieldDescriptor::new(
                    "filter",
                    "Only deliveries matching",
                    FieldKind::Filter {
                        fields: vec![
                            "action".to_string(),
                            "repository.full_name".to_string(),
                            "sender.login".to_string(),
                        ],
                    },
                ),
                FieldDescriptor::new(
                    "secret",
                    "Webhook secret",
                    FieldKind::Secret {
                        placeholder: Some("a long random string".to_string()),
                        generator: true,
                        generator_bytes: 32,
                    },
                )
                .with_help(
                    "The secret you set on the webhook in GitHub. Deliveries are signed with it, \
                     and refused while it is empty.",
                ),
                FieldDescriptor::new(
                    "notes",
                    "Notes",
                    FieldKind::TextArea {
                        placeholder: Some("What is this for?".to_string()),
                    },
                )
                .with_help("Only ever shown here, to remind you why this exists."),
            ],
        },
        WorkflowTypeDescriptor {
            id: "todoist_events".to_string(),
            name: "Todoist events".to_string(),
            description: "Act on changes Todoist reports for a linked account.".to_string(),
            documentation: String::new(),
            trigger: WorkflowTrigger::RoutedWebhook {
                source: "todoist".to_string(),
            },
            fields: vec![
                FieldDescriptor::new(
                    "todoist.connection",
                    "Todoist account",
                    FieldKind::Connection {
                        provider: "todoist".to_string(),
                        connection_kind: None,
                    },
                )
                .required(),
                FieldDescriptor::new(
                    "label",
                    "Label to watch",
                    FieldKind::Text {
                        placeholder: Some("waiting".to_string()),
                    },
                ),
            ],
        },
    ]
}

/// Sample workflows: one of each trigger, one of them paused.
///
/// Health is derived from [`workflow_runs`] rather than written out again, so
/// the pill on a row and the panel it opens cannot disagree.
pub fn workflows() -> Vec<Workflow> {
    let now = Utc::now();
    let health = |id: WorkflowId| workflow_runs(&id.to_string()).map(|state| state.health());

    vec![
        Workflow {
            id: WorkflowId::from_entropy(1),
            type_id: "rss".to_string(),
            name: "https://blog.sierrasoftworks.com/feed.xml".to_string(),
            enabled: true,
            config: json!({
                "feed": { "url": "https://blog.sierrasoftworks.com/feed.xml" },
                "filter": "title contains \"release\"",
                "todoist": {
                    "connection": ConnectionId::from_entropy(1).to_string(),
                    "project": "2203306141",
                    "priority": 2
                },
                "include_summary": true
            }),
            schedule: Some("0 */6 * * *".to_string()),
            webhook_path: None,
            resettable: true,
            created_at: now - Duration::days(90),
            updated_at: now - Duration::days(3),
            last_run: Some(now - Duration::hours(2)),
            next_run: Some(now + Duration::hours(4)),
            health: health(WorkflowId::from_entropy(1)),
        },
        Workflow {
            id: WorkflowId::from_entropy(2),
            type_id: "github_notifications".to_string(),
            name: "GitHub notifications".to_string(),
            enabled: false,
            config: json!({
                "github": { "connection": ConnectionId::from_entropy(2).to_string() },
                "reason": "review_requested",
                "todoist": {
                    "connection": ConnectionId::from_entropy(1).to_string(),
                    "project": "2203306142"
                }
            }),
            schedule: Some("*/15 * * * *".to_string()),
            webhook_path: None,
            resettable: true,
            created_at: now - Duration::days(45),
            updated_at: now - Duration::days(1),
            last_run: Some(now - Duration::days(1)),
            next_run: None,
            health: health(WorkflowId::from_entropy(2)),
        },
        Workflow {
            id: WorkflowId::from_entropy(3),
            type_id: "github_webhook".to_string(),
            name: "GitHub webhook".to_string(),
            enabled: true,
            config: json!({
                "event": "release",
                "filter": "action == \"published\"",
                "notes": "Announces new releases in the team channel."
            }),
            schedule: None,
            webhook_path: Some("/webhooks/github/8f14e45fceea167a5a36dedd4bea2543".to_string()),
            resettable: false,
            created_at: now - Duration::days(7),
            updated_at: now - Duration::days(7),
            last_run: Some(now - Duration::minutes(20)),
            next_run: None,
            health: health(WorkflowId::from_entropy(3)),
        },
    ]
}

/// A history of the things worth keeping: a workflow whose health changed, a
/// delivery that was turned away, and the changes somebody made by hand.
///
/// Deliberately without an entry per run. That is the arrangement this replaced,
/// and a fixture that still showed one would have the page reviewed against
/// behaviour the agent no longer has.
pub fn audit() -> Vec<AuditRecord> {
    let now = Utc::now();
    let tenant = TenantId::new("demo").expect("the demo tenant name is valid");

    // Newest first, as the agent returns them, and with descending ids so that
    // paging behaves the way it does against a real log.
    let mut id = 40;
    let mut entry = |occurred_at,
                     category,
                     action: &str,
                     outcome,
                     subject: Option<String>,
                     message: Option<&str>,
                     detail| {
        id -= 1;
        AuditRecord {
            id,
            tenant: tenant.clone(),
            occurred_at,
            category,
            action: action.to_string(),
            outcome,
            subject,
            actor: None,
            message: message.map(ToString::to_string),
            detail,
        }
    };

    let rss = WorkflowId::from_entropy(1).to_string();
    let notifications = WorkflowId::from_entropy(2).to_string();
    let webhook = WorkflowId::from_entropy(3).to_string();

    vec![
        entry(
            now - Duration::minutes(12),
            AuditCategory::WorkflowRun,
            "started-failing",
            AuditOutcome::Failure,
            Some(rss.clone()),
            Some(
                "The feed at https://blog.sierrasoftworks.com/feed.xml did not respond within 30 seconds.",
            ),
            None,
        ),
        entry(
            now - Duration::hours(3),
            AuditCategory::WebhookDelivery,
            "received",
            AuditOutcome::Denied,
            Some(webhook.clone()),
            Some("Refused a delivery whose address did not match this workflow."),
            None,
        ),
        entry(
            now - Duration::hours(9),
            AuditCategory::WorkflowRun,
            "recovered",
            AuditOutcome::Success,
            Some(webhook),
            Some("This workflow is working again, after 4 failed runs."),
            None,
        ),
        entry(
            now - Duration::days(1),
            AuditCategory::WorkflowConfig,
            "paused",
            AuditOutcome::Success,
            Some(notifications),
            Some("Paused this workflow."),
            None,
        ),
        entry(
            now - Duration::days(3),
            AuditCategory::WorkflowConfig,
            "updated",
            AuditOutcome::Success,
            Some(rss),
            Some("Changed this workflow's settings."),
            None,
        ),
        entry(
            now - Duration::days(3),
            AuditCategory::Connection,
            "replaced-key",
            AuditOutcome::Success,
            Some(ConnectionId::from_entropy(4).to_string()),
            Some("Replaced the API key for 'Budget'."),
            None,
        ),
        entry(
            now - Duration::days(5),
            AuditCategory::Authentication,
            "signed-in",
            AuditOutcome::Success,
            Some("demo@example.com".to_string()),
            None,
            None,
        ),
    ]
}

/// What each workflow's last runs looked like.
///
/// Between them: one that is failing and shows the payload it failed on, one
/// that is working but failed earlier in the night, and one that has never run.
pub fn workflow_runs(workflow: &str) -> Option<RunState> {
    let now = Utc::now();

    let report = |finished: chrono::DateTime<Utc>,
                  outcome,
                  message: Option<&str>,
                  input: Option<serde_json::Value>| RunReport {
        started_at: finished - Duration::milliseconds(840),
        finished_at: finished,
        outcome,
        message: message.map(ToString::to_string),
        input,
    };

    let rss = report(
        now - Duration::minutes(12),
        RunOutcome::Failed,
        Some(
            "The feed at https://blog.sierrasoftworks.com/feed.xml did not respond within 30 seconds.",
        ),
        Some(json!({
            "feed": { "url": "https://blog.sierrasoftworks.com/feed.xml" },
            "include_summary": true,
        })),
    );

    // A delivery, redacted the way the agent redacts one before storing it.
    let delivery = |action: &str, finished, outcome, message: Option<&str>| {
        report(
            finished,
            outcome,
            message,
            Some(json!({
                "workflow": WorkflowId::from_entropy(3).to_string(),
                "event": {
                    "headers": {
                        "x-github-event": "release",
                        "x-hub-signature-256": "<redacted>",
                        "content-type": "application/json",
                    },
                    "body": format!("{{\"action\":\"{action}\",\"release\":{{\"tag_name\":\"v2.0.2\"}}}}"),
                },
            })),
        )
    };

    match workflow {
        id if id == WorkflowId::from_entropy(1).to_string() => Some(RunState {
            last: rss.clone(),
            last_failure: Some(rss),
            consecutive_failures: 3,
        }),
        id if id == WorkflowId::from_entropy(3).to_string() => Some(RunState {
            last: delivery(
                "published",
                now - Duration::hours(1),
                RunOutcome::Succeeded,
                None,
            ),
            last_failure: Some(delivery(
                "created",
                now - Duration::hours(9),
                RunOutcome::Failed,
                Some("Todoist refused the request: 403 Forbidden."),
            )),
            consecutive_failures: 0,
        }),
        _ => None,
    }
}

/// The integrations demo mode pretends the agent has configured.
pub fn integrations() -> Vec<IntegrationInfo> {
    vec![
        IntegrationInfo {
            id: "github".to_string(),
            name: "GitHub".to_string(),
        },
        IntegrationInfo {
            id: "spotify".to_string(),
            name: "Spotify".to_string(),
        },
    ]
}

/// The accounts connected to each integration. The second integration is
/// deliberately left unconnected so the "Not connected." state is reachable.
pub fn integration_connections(integration: &str) -> Vec<Connection> {
    match integration {
        "github" => vec![
            Connection::new("41326112", "SierraSoftworks")
                .with_kind("Organization")
                .with_detail("Installed 30 days ago"),
            Connection::new("41326113", "notheotherben").with_kind("User"),
        ],
        _ => vec![],
    }
}

/// The choices a picker fetches through a linked account.
pub fn connection_options(source: &str, parent: Option<&str>) -> Vec<OptionItem> {
    match source {
        "projects" => vec![
            OptionItem::new("2203306140", "Inbox").as_default(),
            OptionItem::new("2203306141", "Software").with_color("#299438"),
            OptionItem::new("2203306142", "Home").with_color("#eb8909"),
        ],
        // Scoped by the chosen project, which is the whole reason `parent`
        // exists: without it every section in the workspace would be offered.
        "sections" => match parent {
            Some("2203306141") => vec![
                OptionItem::new("7025", "Backlog"),
                OptionItem::new("7026", "In progress"),
            ],
            Some("2203306142") => vec![OptionItem::new("7101", "Errands")],
            _ => vec![],
        },
        _ => vec![],
    }
}

const RSS_DOCUMENTATION: &str = r#"## How this works

Automate polls the feed on the schedule you choose and creates a Todoist task
for every item it has not seen before. Items are remembered by their feed
identifier, so a feed that republishes an item will not create a duplicate.

### Filtering

The filter runs over each item before a task is created. It can refer to
`title`, `link`, `summary` and `author`:

```text
title contains "release" and not author == "bot"
```

Leaving the filter empty takes every item.

See the [feed reader documentation](https://github.com/SierraSoftworks/automate)
for the full expression syntax.
"#;

const WEBHOOK_DOCUMENTATION: &str = r#"## Pointing GitHub at this workflow

1. Copy the address below.
2. In GitHub, open **Settings → Webhooks → Add webhook** for the repository or
   organisation you want to watch.
3. Paste the address into **Payload URL**, set the content type to
   `application/json`, and choose the events this workflow should receive.

The address carries its own secret, so treat it like a credential. If it leaks,
rotate it here and the old one stops working immediately.
"#;
