use std::borrow::Cow;
use std::fmt::Display;

use hmac::{Hmac, KeyInit, Mac};

use crate::jobs::{
    GitHubAttentionConfig, GitHubAttentionWorkflow, GitHubAutoMergeConfig, GitHubAutoMergeWorkflow,
    GitHubNotificationsRefreshWorkflow,
};
use crate::prelude::*;

type HmacSha256 = Hmac<Sha256>;

use sha2::Sha256;

/// The webhook events which can add to, or resolve, a GitHub notification
/// thread. Anything else (pushes, statuses, deployments) never moves the
/// notifications inbox, so it would only cost us a wasted fetch.
const NOTIFICATION_EVENTS: &[&str] = &[
    "code_scanning_alert",
    "dependabot_alert",
    "discussion",
    "discussion_comment",
    "issue_comment",
    "issues",
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "release",
    "repository_vulnerability_alert",
    "secret_scanning_alert",
    "security_advisory",
    "workflow_run",
];

/// Events carrying a comment or review on an issue or pull request.
const COMMENT_EVENTS: &[&str] = &[
    "issue_comment",
    "pull_request_review",
    "pull_request_review_comment",
];

/// Events carrying a repository security alert. The deprecated
/// `repository_vulnerability_alert` is excluded because its payload predates
/// the shape the others share.
const SECURITY_ALERT_EVENTS: &[&str] = &[
    "code_scanning_alert",
    "dependabot_alert",
    "secret_scanning_alert",
];

/// Alert, assignment and subject actions which mean the subject no longer needs
/// attention, so any task tracking it is completed instead of raised.
const RESOLVING_ACTIONS: &[&str] = &[
    "auto_dismissed",
    "closed",
    "closed_by_user",
    "dismissed",
    "fixed",
    "resolved",
    "unassigned",
];

/// What one person asked us to do with deliveries from a GitHub App installation.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GitHubWebhookConfig {
    /// What to call this workflow, so it can be told apart from the others in a
    /// list of them.
    pub name: String,

    /// The GitHub App installation whose deliveries this workflow handles, which
    /// is to say the account the App was installed on.
    ///
    /// Optional in storage although the form insists on it, because a workflow
    /// written before this field existed still has to load; the same compromise
    /// [`crate::publishers::TodoistTarget`] makes for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<automate_api::ConnectionId>,

    /// How this workflow treats `pull_request` events, including whether it
    /// treats them at all.
    ///
    /// This used to be an `Option`, whose presence was the switch. That read
    /// tidily and cost the settings inside it any way of being configured:
    /// [`crate::config_path!`] borrows the field it names, and there is nothing
    /// to borrow through a `None`. An explicit `enabled` says the same thing
    /// while leaving `auto_merge.filter` a path that exists.
    #[serde(default)]
    pub auto_merge: GitHubAutoMergeConfig,

    /// Whether, and how, comments, assignments and security alerts become
    /// Todoist reminders. An explicit switch for the same reason as
    /// `auto_merge`.
    #[serde(default)]
    pub attention: GitHubAttentionConfig,
}

impl Display for GitHubWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github/{}", self.name)
    }
}

/// The job which processes one workflow's routed GitHub App delivery.
///
/// GitHub delivers every event type to a single endpoint, so this job performs
/// the work which is common to all of them - signature verification and event
/// type routing - and then hands the parsed payload to the dedicated job queue
/// which knows how to act on it.
#[derive(Clone)]
pub struct GitHubWebhook;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Listens to one GitHub App installation's deliveries and acts on them as they
arrive, rather than by polling. Automate receives these through the webhook
configured on the GitHub App and routes them by installation automatically;
there is no webhook to configure for this workflow.

Two independent things can
be switched on, and both are off until you say otherwise:

- **Auto-merge** turns on GitHub's own auto-merge for pull requests you select,
  so they merge themselves once their checks pass.
- **Reminders** raise a Todoist task when a comment, an assignment or a
  security alert wants your attention, and complete it when the subject is
  dealt with.

With both off this workflow still keeps the GitHub notifications inbox in sync,
which is the only thing it does without being asked.

## Auto-merge

**Enable auto-merge** is the switch; nothing below it happens while it is off.

**Auto-merge these pull requests** selects which ones to act on, matching on
`action`, `author`, `sender`, `title`, `draft`, `repository`,
`repository_name`, `repository_owner` and `private`. The default is the case
this exists for:

```
action == "opened" && author in ["dependabot[bot]", "dependabot-preview[bot]"]
```

**Approve them too** additionally leaves an approving review. Repositories with
a required-review branch protection rule need one before auto-merge can
complete, so it is necessary there and gratuitous everywhere else — it spends a
real approval on somebody else's diff. **Approval message** is the body of that
review.

Auto-merge writes to your repositories, so it needs the GitHub App installed on
the account that owns them, named by **GitHub installation**.

When a repository does not allow auto-merge, a Todoist task asks you to turn on
its **Allow auto-merge** setting — unless the repository is private, where the
setting may not be available at all, in which case the task asks you to merge
that pull request by hand instead.

## Reminders

**File reminders** is the switch. Below it are three filters, one per kind of
event, each matching on `kind`, `event`, `action`, `resolved`, `repository`,
`repository_owner`, `repository_name`, `number`, `title`, `author`, `assignee`,
`subject_author`, `body` and `severity`.

- **Comments** defaults to everything except Dependabot's own commentary.
  `subject_author == "your-username"` narrows it to threads on your own issues
  and pull requests, which is usually what people mean.
- **Assignments** defaults to `false` — nothing — because only you know which
  account is yours. `assignee == "your-username"` is the answer.
- **Security alerts** defaults to `true`, all of them. Narrow it with
  `severity in ["critical", "high"]` if that is too much.

Comments and assignments on the same issue or pull request collapse onto one
task, so a busy thread does not become a wall of them. Security alerts get one
task each, because each needs its own fix.
"#;

impl GitHubWebhook {
    /// Verifies the `X-Hub-Signature-256` header, which GitHub populates with
    /// `sha256=<hex>` where the digest is an HMAC-SHA256 of the raw request
    /// body keyed with the webhook secret.
    ///
    /// See https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
    pub(crate) fn verify_signature(
        secret: &str,
        body: &str,
        signature_header: &str,
    ) -> Result<(), human_errors::Error> {
        let signature = signature_header.strip_prefix("sha256=").ok_or_else(|| {
            human_errors::user(
                "The X-Hub-Signature-256 header was not in the expected 'sha256=<hex>' format.",
                &[
                    "Ensure that you are only sending GitHub webhooks to this endpoint.",
                    "Check that the webhook is configured to use the SHA-256 signature algorithm.",
                ],
            )
        })?;

        let expected_signature = hex::decode(signature).or_user_err(&[
            "The signature in the X-Hub-Signature-256 header is not valid hex.",
            "Ensure that you are only sending GitHub webhooks to this endpoint.",
        ])?;

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).wrap_user_err(
            "Failed to create HMAC instance with the provided secret.",
            &["Ensure that connections.github.app.webhook_secret is configured."],
        )?;

        mac.update(body.as_bytes());

        mac.verify_slice(&expected_signature).wrap_user_err(
            "Webhook signature verification failed (signatures did not match).".to_string(),
            &[
                "Ensure that connections.github.app.webhook_secret matches the secret set on the GitHub App webhook.",
            ],
        )?;

        Ok(())
    }

    fn header<'a>(event: &'a WebhookEvent, name: &str) -> Option<&'a str> {
        event
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    async fn track_installation(
        event: &WebhookEvent,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        let event: GitHubInstallationEvent = event.json()?;
        let installation = crate::services::GitHubInstallation {
            id: event.installation.id,
            account: event.installation.account.login.clone(),
            account_type: event.installation.account.type_.clone(),
        };

        match event.action.as_str() {
            "created" | "new_permissions_accepted" | "unsuspend" => {
                info!(
                    "GitHub App installed on '{}' (installation {}).",
                    installation.account, installation.id
                );
                crate::integrations::github_app::record_installation(&installation, services).await
            }
            "deleted" | "suspend" => {
                info!("GitHub App removed from '{}'.", installation.account);
                crate::integrations::github_app::forget_installation(
                    &installation.account,
                    services,
                )
                .await
            }
            other => {
                debug!("Ignoring GitHub installation event '{other}'.");
                Ok(())
            }
        }
    }
}

crate::register_job!(GitHubWebhook);
crate::register_workflow_type!(GitHubWebhook);

impl crate::workflows::ConfigurableWorkflow for GitHubWebhook {
    type ConfigType = GitHubWebhookConfig;

    fn type_id() -> &'static str {
        "github"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "GitHub".to_string(),
            description:
                "Reacts to what happens in your GitHub organisation, as it happens rather than on a poll."
                    .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::RoutedWebhook {
                source: "github".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(GitHubWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("SierraSoftworks".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(GitHubWebhookConfig: connection),
                    "GitHub installation",
                    FieldKind::Connection {
                        provider: crate::integrations::github_app::GITHUB_PROVIDER.to_string(),
                        connection_kind: Some(automate_api::ConnectionKind::GitHubApp),
                    },
                )
                .with_help(
                    "Which installation of the GitHub App this workflow serves — the GitHub account you installed it on. Its deliveries are the ones this workflow handles, and its repositories are the ones auto-merge can write to.",
                )
                .required(),
            ]
            .into_iter()
            .chain(Self::auto_merge_fields())
            .chain(Self::attention_fields())
            .collect(),
        }
    }
}

impl GitHubWebhook {
    /// The names a `pull_request` delivery exposes to a filter, taken from the
    /// [`Filterable`] impl on [`GitHubPullRequestEvent`] so the editor suggests
    /// what the evaluator will actually answer to.
    fn pull_request_filter_fields() -> Vec<String> {
        [
            "action",
            "author",
            "sender",
            "title",
            "draft",
            "repository",
            "repository_name",
            "repository_owner",
            "private",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    /// The names an attention event exposes to a filter, from the [`Filterable`]
    /// impl on [`GitHubAttentionEvent`].
    fn attention_filter_fields() -> Vec<String> {
        [
            "kind",
            "event",
            "action",
            "resolved",
            "repository",
            "repository_owner",
            "repository_name",
            "number",
            "title",
            "author",
            "assignee",
            "subject_author",
            "body",
            "severity",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    /// The settings [`GitHubAutoMergeWorkflow`] is handed, as form fields.
    ///
    /// Every default here is the constant the struct's own `serde(default)` uses,
    /// so a form left untouched and a configuration that omitted the section
    /// describe the same behaviour rather than two behaviours that happen to
    /// coincide today.
    fn auto_merge_fields() -> [automate_api::FieldDescriptor; 4] {
        use automate_api::{FieldDescriptor, FieldKind};

        [
            FieldDescriptor::new(
                crate::config_path!(GitHubWebhookConfig: auto_merge.enabled),
                "Enable auto-merge",
                FieldKind::Boolean,
            )
            .with_help(
                "Turn on GitHub's own auto-merge for the pull requests below, so they merge themselves once their checks pass. Off until you say otherwise, because this writes to your repositories.",
            )
            .with_default(false),
            FieldDescriptor::new(
                crate::config_path!(GitHubWebhookConfig: auto_merge.filter),
                "Auto-merge these pull requests",
                FieldKind::Filter {
                    fields: Self::pull_request_filter_fields(),
                },
            )
            .with_help(
                "Which pull requests to act on. The default is newly opened Dependabot ones, which is the case this exists for.",
            )
            .with_default(crate::jobs::DEFAULT_AUTO_MERGE_FILTER),
            FieldDescriptor::new(
                crate::config_path!(GitHubWebhookConfig: auto_merge.approve),
                "Approve them too",
                FieldKind::Boolean,
            )
            .with_help(
                "Leave an approving review as well, which repositories with a required-review branch protection rule need before auto-merge can complete. Leave this off unless you have such a rule: it spends a real approval on somebody else's diff.",
            )
            .with_default(false),
            FieldDescriptor::new(
                crate::config_path!(GitHubWebhookConfig: auto_merge.approval_message),
                "Approval message",
                FieldKind::TextArea {
                    placeholder: Some(crate::jobs::DEFAULT_APPROVAL_MESSAGE.into()),
                },
            )
            .with_help("The body of the review left when approval is switched on.")
            .with_default(crate::jobs::DEFAULT_APPROVAL_MESSAGE),
        ]
    }

    /// The settings [`GitHubAttentionWorkflow`] is handed, as form fields.
    fn attention_fields() -> [automate_api::FieldDescriptor; 4] {
        use automate_api::{FieldDescriptor, FieldKind};

        [
            FieldDescriptor::new(
                crate::config_path!(GitHubWebhookConfig: attention.enabled),
                "File reminders",
                FieldKind::Boolean,
            )
            .with_help(
                "Raise a Todoist task when something in these repositories wants your attention. Off until you say otherwise, so an upgrade cannot start filling your inbox.",
            )
            .with_default(false),
            FieldDescriptor::new(
                crate::config_path!(GitHubWebhookConfig: attention.comments),
                "Remind me about these comments",
                FieldKind::Filter {
                    fields: Self::attention_filter_fields(),
                },
            )
            .with_help(
                "Which comments and reviews are worth a task. The default is everything except Dependabot's own commentary; `subject_author == \"you\"` narrows it to your own issues and pull requests.",
            )
            .with_default(crate::jobs::DEFAULT_COMMENT_FILTER),
            FieldDescriptor::new(
                crate::config_path!(GitHubWebhookConfig: attention.assignments),
                "Remind me about these assignments",
                FieldKind::Filter {
                    fields: Self::attention_filter_fields(),
                },
            )
            .with_help(
                "Which assignments are worth a task. Nothing by default, because only you know which account is yours — `assignee == \"you\"` is the usual answer.",
            )
            .with_default(crate::jobs::DEFAULT_ASSIGNMENT_FILTER),
            FieldDescriptor::new(
                crate::config_path!(GitHubWebhookConfig: attention.security_alerts),
                "Remind me about these security alerts",
                FieldKind::Filter {
                    fields: Self::attention_filter_fields(),
                },
            )
            .with_help(
                "Which Dependabot, code scanning and secret scanning alerts are worth a task. All of them by default; `severity in [\"critical\", \"high\"]` if that is too much.",
            )
            .with_default(crate::jobs::DEFAULT_SECURITY_ALERT_FILTER),
        ]
    }
}

impl Job for GitHubWebhook {
    type JobType = crate::webhooks::WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/github"
    }

    #[instrument("webhooks.github.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the delivery, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<GitHubWebhookConfig>(services).await? else {
            return Ok(());
        };

        let event = &job.event;

        // GitHub identifies each delivery with a GUID which is preserved across
        // its own retries, making it a natural idempotency key for the jobs we
        // fan out to.
        let delivery = Self::header(event, "x-github-delivery")
            .map(|id| Cow::Owned(format!("{id}/{}", job.workflow)));
        let event_type = Self::header(event, "x-github-event").unwrap_or_default();

        if event_type == "ping" {
            info!("Received a GitHub webhook ping event.");
            return Ok(());
        }

        // GitHub reports installs and uninstalls directly, which keeps the
        // registry accurate even when someone installs the App from GitHub
        // rather than through the wizard.
        if event_type == "installation" {
            return Self::track_installation(event, services).await;
        }

        let mut handled = false;

        if event_type == "pull_request" && config.auto_merge.enabled {
            let pull_request: GitHubPullRequestEvent = event.json()?;
            GitHubAutoMergeWorkflow::dispatch(
                crate::jobs::GitHubAutoMergeTask {
                    config: config.auto_merge.clone(),
                    // Which installation this workflow serves, so the job mints
                    // its token from what its owner chose rather than from what
                    // the delivery claims to be.
                    connection: config.connection,
                    event: pull_request,
                },
                delivery.clone(),
                services,
            )
            .await?;
            handled = true;
        }

        if config.attention.enabled {
            // A payload we cannot interpret is skipped rather than raised, so that
            // an unmodelled variant cannot poison the queue by retrying forever.
            match GitHubAttentionEvent::parse(event_type, event) {
                Ok(Some(attention)) => {
                    GitHubAttentionWorkflow::dispatch(
                        crate::jobs::GitHubAttentionTask {
                            config: config.attention.clone(),
                            event: attention,
                        },
                        delivery,
                        services,
                    )
                    .await?;
                    handled = true;
                }
                Ok(None) => {}
                Err(err) => warn!(
                    "Could not interpret the '{event_type}' payload as an attention event; skipping it: {err}"
                ),
            }
        }

        if NOTIFICATION_EVENTS.contains(&event_type) {
            GitHubNotificationsRefreshWorkflow::schedule(event_type, services).await?;
            handled = true;
        }

        if !handled {
            debug!("Received an unsupported GitHub webhook event '{event_type}'; ignoring.");
        }

        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubPullRequestEvent {
    pub action: String,
    pub number: u64,
    pub pull_request: GitHubPullRequest,
    pub repository: GitHubWebhookRepository,
    #[serde(default)]
    pub sender: Option<GitHubWebhookUser>,

    /// Present when the delivery came from a GitHub App installation, which is
    /// what lets management calls authenticate as the App rather than a PAT.
    #[serde(default)]
    pub installation: Option<GitHubWebhookInstallation>,
}

impl Display for GitHubPullRequestEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}#{} ({})",
            self.repository.full_name, self.number, self.action
        )
    }
}

impl Filterable for GitHubPullRequestEvent {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        match key {
            "action" => self.action.as_str().into(),
            "author" => self
                .pull_request
                .user
                .as_ref()
                .map(|u| u.login.as_str().into())
                .unwrap_or(crate::filter::FilterValue::Null),
            "sender" => self
                .sender
                .as_ref()
                .map(|u| u.login.as_str().into())
                .unwrap_or(crate::filter::FilterValue::Null),
            "title" => self.pull_request.title.as_str().into(),
            "draft" => crate::filter::FilterValue::Bool(self.pull_request.draft),
            "repository" => self.repository.full_name.as_str().into(),
            "repository_name" => self.repository.name.as_str().into(),
            "repository_owner" => self.repository.owner.login.as_str().into(),
            "private" => crate::filter::FilterValue::Bool(self.repository.private),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubPullRequest {
    /// The GraphQL node ID, which is what the auto-merge and review mutations
    /// address the pull request by.
    pub node_id: String,
    pub html_url: String,
    pub title: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub user: Option<GitHubWebhookUser>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubWebhookRepository {
    pub name: String,
    pub full_name: String,
    #[serde(default)]
    pub private: bool,
    pub owner: GitHubWebhookUser,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubWebhookUser {
    pub login: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubWebhookInstallation {
    pub id: u64,
}

#[derive(Deserialize)]
struct GitHubInstallationEvent {
    action: String,
    installation: GitHubInstallationDetail,
}

#[derive(Deserialize)]
struct GitHubInstallationDetail {
    id: u64,
    account: GitHubInstallationAccount,
}

#[derive(Deserialize)]
struct GitHubInstallationAccount {
    login: String,
    #[serde(rename = "type", default)]
    type_: String,
}

/// What prompted a subject to need the user's attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubAttentionKind {
    Comment,
    Assignment,
    SecurityAlert,

    /// The subject was closed or merged, which only ever retires a task.
    Closure,
}

impl GitHubAttentionKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Assignment => "assignment",
            Self::SecurityAlert => "security_alert",
            Self::Closure => "closure",
        }
    }
}

/// A GitHub event which suggests an issue, pull request or repository needs the
/// user's attention.
///
/// The various source events (comments, reviews, assignments and the three
/// security alert flavours) are normalised into this one shape so that a single
/// filter surface and job can serve them all.
#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubAttentionEvent {
    pub kind: GitHubAttentionKind,
    pub event: String,
    pub action: String,

    /// Whether the subject has been dealt with, in which case any task tracking
    /// it is completed rather than raised.
    pub resolved: bool,

    pub repository: String,
    pub repository_owner: String,
    pub repository_name: String,

    /// The issue/pull request number, or the alert number.
    pub number: u64,
    pub title: String,
    /// The issue, pull request or alert this concerns. Comments deliberately
    /// point here rather than at the comment permalink, so that the task's title
    /// is identical to the one the notification collector would render for the
    /// same subject.
    pub url: String,

    /// The permalink to the comment or review which prompted this, if any.
    #[serde(default)]
    pub comment_url: Option<String>,

    /// Whoever's action prompted this: the comment author, or the person who
    /// changed the assignment or alert.
    pub actor: Option<String>,
    pub assignee: Option<String>,

    /// The author of the issue or pull request being commented on, which is
    /// what makes "comments on my pull requests" expressible as a filter.
    pub subject_author: Option<String>,

    pub body: Option<String>,
    pub severity: Option<String>,
}

impl Display for GitHubAttentionEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}#{} ({}/{})",
            self.repository,
            self.number,
            self.kind.as_str(),
            self.action
        )
    }
}

impl Filterable for GitHubAttentionEvent {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        match key {
            "kind" => self.kind.as_str().into(),
            "event" => self.event.as_str().into(),
            "action" => self.action.as_str().into(),
            "resolved" => crate::filter::FilterValue::Bool(self.resolved),
            "repository" => self.repository.as_str().into(),
            "repository_owner" => self.repository_owner.as_str().into(),
            "repository_name" => self.repository_name.as_str().into(),
            "number" => crate::filter::FilterValue::Number(self.number as f64),
            "title" => self.title.as_str().into(),
            "author" => optional(self.actor.as_ref()),
            "assignee" => optional(self.assignee.as_ref()),
            "subject_author" => optional(self.subject_author.as_ref()),
            "body" => optional(self.body.as_ref()),
            "severity" => optional(self.severity.as_ref()),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

fn optional(value: Option<&String>) -> crate::filter::FilterValue<'_> {
    value
        .map(|v| v.as_str().into())
        .unwrap_or(crate::filter::FilterValue::Null)
}

impl GitHubAttentionEvent {
    /// The Todoist key this event maps onto. Comments and assignments collapse
    /// onto their issue or pull request so that a busy thread yields one task,
    /// while each security alert is tracked separately because each needs its
    /// own fix.
    pub fn unique_key(&self) -> String {
        match self.kind {
            GitHubAttentionKind::SecurityAlert => format!(
                "github/attention/{}/{}/{}",
                self.repository, self.event, self.number
            ),
            _ => crate::jobs::subject_key(&self.repository, self.number),
        }
    }

    /// Normalises a webhook delivery, returning `None` when the event carries
    /// nothing which warrants attention.
    pub fn parse(
        event_type: &str,
        job: &WebhookEvent,
    ) -> Result<Option<Self>, human_errors::Error> {
        if COMMENT_EVENTS.contains(&event_type) {
            let payload: GitHubActivityPayload = job.json()?;

            // Edits and deletions of an existing comment do not represent new
            // activity to respond to.
            if !matches!(payload.action.as_str(), "created" | "submitted") {
                return Ok(None);
            }

            let (Some(subject), Some(comment)) = (
                payload.subject(),
                payload.comment.as_ref().or(payload.review.as_ref()),
            ) else {
                return Ok(None);
            };

            return Ok(Some(Self {
                kind: GitHubAttentionKind::Comment,
                event: event_type.to_string(),
                action: payload.action.clone(),
                resolved: false,
                repository: payload.repository.full_name.clone(),
                repository_owner: payload.repository.owner.login.clone(),
                repository_name: payload.repository.name.clone(),
                number: subject.number,
                title: subject.title.clone(),
                url: subject.html_url.clone(),
                comment_url: comment.html_url.clone(),
                actor: comment
                    .user
                    .as_ref()
                    .or(payload.sender.as_ref())
                    .map(|u| u.login.clone()),
                assignee: None,
                subject_author: subject.user.as_ref().map(|u| u.login.clone()),
                body: comment.body.clone(),
                severity: None,
            }));
        }

        if SECURITY_ALERT_EVENTS.contains(&event_type) {
            let payload: GitHubActivityPayload = job.json()?;
            let Some(alert) = payload.alert.as_ref() else {
                return Ok(None);
            };

            return Ok(Some(Self {
                kind: GitHubAttentionKind::SecurityAlert,
                event: event_type.to_string(),
                action: payload.action.clone(),
                resolved: RESOLVING_ACTIONS.contains(&payload.action.as_str()),
                repository: payload.repository.full_name.clone(),
                repository_owner: payload.repository.owner.login.clone(),
                repository_name: payload.repository.name.clone(),
                number: alert.number,
                title: alert.summary(),
                url: alert.html_url.clone().unwrap_or_else(|| {
                    format!(
                        "https://github.com/{}/security",
                        payload.repository.full_name
                    )
                }),
                comment_url: None,
                actor: payload.sender.as_ref().map(|u| u.login.clone()),
                assignee: None,
                subject_author: None,
                body: alert.detail(),
                severity: alert.severity(),
            }));
        }

        if matches!(event_type, "issues" | "pull_request") {
            let payload: GitHubActivityPayload = job.json()?;

            let kind = match payload.action.as_str() {
                "assigned" | "unassigned" => GitHubAttentionKind::Assignment,
                // A merged pull request arrives as `closed` too, with `merged`
                // set, so both cases are covered by the one action.
                "closed" => GitHubAttentionKind::Closure,
                _ => return Ok(None),
            };

            let Some(subject) = payload.subject() else {
                return Ok(None);
            };

            return Ok(Some(Self {
                kind,
                event: event_type.to_string(),
                action: payload.action.clone(),
                resolved: RESOLVING_ACTIONS.contains(&payload.action.as_str()),
                repository: payload.repository.full_name.clone(),
                repository_owner: payload.repository.owner.login.clone(),
                repository_name: payload.repository.name.clone(),
                number: subject.number,
                title: subject.title.clone(),
                url: subject.html_url.clone(),
                comment_url: None,
                actor: payload.sender.as_ref().map(|u| u.login.clone()),
                assignee: payload.assignee.as_ref().map(|u| u.login.clone()),
                subject_author: subject.user.as_ref().map(|u| u.login.clone()),
                body: None,
                severity: None,
            }));
        }

        Ok(None)
    }
}

/// A permissive view over the issue, pull request, comment, assignment and
/// alert payloads, which overlap enough to share one model.
#[derive(Deserialize)]
struct GitHubActivityPayload {
    action: String,
    #[serde(default)]
    issue: Option<GitHubSubject>,
    #[serde(default)]
    pull_request: Option<GitHubSubject>,
    #[serde(default)]
    comment: Option<GitHubComment>,
    #[serde(default)]
    review: Option<GitHubComment>,
    #[serde(default)]
    assignee: Option<GitHubWebhookUser>,
    #[serde(default)]
    alert: Option<GitHubAlert>,
    repository: GitHubWebhookRepository,
    #[serde(default)]
    sender: Option<GitHubWebhookUser>,
}

impl GitHubActivityPayload {
    fn subject(&self) -> Option<&GitHubSubject> {
        self.issue.as_ref().or(self.pull_request.as_ref())
    }
}

#[derive(Deserialize)]
struct GitHubSubject {
    number: u64,
    title: String,
    html_url: String,
    #[serde(default)]
    user: Option<GitHubWebhookUser>,
}

#[derive(Deserialize)]
struct GitHubComment {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    user: Option<GitHubWebhookUser>,
}

/// The union of the Dependabot, code scanning and secret scanning alert
/// payloads, each of which describes itself through a different field.
#[derive(Deserialize)]
struct GitHubAlert {
    number: u64,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    security_advisory: Option<GitHubAdvisory>,
    #[serde(default)]
    dependency: Option<GitHubDependency>,
    #[serde(default)]
    rule: Option<GitHubRule>,
    #[serde(default)]
    secret_type_display_name: Option<String>,
    #[serde(default)]
    secret_type: Option<String>,
}

impl GitHubAlert {
    fn summary(&self) -> String {
        if let Some(advisory) = self.security_advisory.as_ref() {
            return match self.dependency.as_ref().and_then(|d| d.package.as_ref()) {
                Some(package) => format!("{} in {}", advisory.summary, package.name),
                None => advisory.summary.clone(),
            };
        }

        if let Some(rule) = self.rule.as_ref()
            && let Some(description) = rule.description.as_ref().or(rule.id.as_ref())
        {
            return description.clone();
        }

        if let Some(secret) = self
            .secret_type_display_name
            .as_ref()
            .or(self.secret_type.as_ref())
        {
            return format!("Leaked {secret}");
        }

        format!("Security alert #{}", self.number)
    }

    fn detail(&self) -> Option<String> {
        self.security_advisory
            .as_ref()
            .and_then(|a| a.description.clone())
    }

    fn severity(&self) -> Option<String> {
        self.security_advisory
            .as_ref()
            .and_then(|a| a.severity.clone())
            .or_else(|| self.rule.as_ref().and_then(|r| r.severity.clone()))
    }
}

#[derive(Deserialize)]
struct GitHubAdvisory {
    summary: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    severity: Option<String>,
}

#[derive(Deserialize)]
struct GitHubDependency {
    #[serde(default)]
    package: Option<GitHubPackage>,
}

#[derive(Deserialize)]
struct GitHubPackage {
    name: String,
}

#[derive(Deserialize)]
struct GitHubRule {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    severity: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::collections::HashMap;

    use crate::{
        webhooks::WebhookDelivery,
        workflow_store::{WorkflowDraft, WorkflowStore},
    };

    const BODY: &str = r#"{"action":"opened","number":1,"pull_request":{"node_id":"PR_node","number":1,"html_url":"https://github.com/example/repo/pull/1","title":"Bump serde","draft":false,"user":{"login":"dependabot[bot]"}},"repository":{"name":"repo","full_name":"example/repo","private":false,"owner":{"login":"example"}},"sender":{"login":"dependabot[bot]"}}"#;

    /// An `issue_comment` delivery, which is what an attention event looks like
    /// arriving. [`BODY`] cannot stand in: it is a `pull_request` payload whose
    /// action is `opened`, which normalises to nothing.
    const COMMENT_BODY: &str = r#"{"action":"created","issue":{"number":7,"title":"Fix the thing","html_url":"https://github.com/example/repo/issues/7","user":{"login":"notheotherben"}},"comment":{"body":"Any update on this?","html_url":"https://github.com/example/repo/issues/7#issuecomment-1","user":{"login":"someone-else"}},"repository":{"name":"repo","full_name":"example/repo","private":false,"owner":{"login":"example"}},"sender":{"login":"someone-else"}}"#;

    /// A section that is switched on but otherwise left at its defaults.
    fn on() -> serde_json::Value {
        serde_json::json!({ "enabled": true })
    }

    /// A section that is switched off, which is also what an omitted one means.
    fn off() -> serde_json::Value {
        serde_json::json!({ "enabled": false })
    }

    fn sign(secret: &str, body: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    /// A delivery of [`BODY`] carrying the given headers.
    fn event(workflow: automate_api::WorkflowId, headers: &[(&str, &str)]) -> WebhookDelivery {
        event_of(workflow, BODY, headers)
    }

    /// A delivery of an arbitrary body, for the tests which need a payload other
    /// than the sample pull request.
    fn event_of(
        workflow: automate_api::WorkflowId,
        body: &str,
        headers: &[(&str, &str)],
    ) -> WebhookDelivery {
        WebhookDelivery {
            workflow,
            event: WebhookEvent {
                body: body.to_string(),
                query: String::new(),
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<HashMap<_, _>>(),
            },
        }
    }

    /// A bare event, for the parsing tests which never reach a stored workflow.
    fn payload(body: serde_json::Value) -> WebhookEvent {
        WebhookEvent {
            body: body.to_string(),
            query: String::new(),
            headers: HashMap::new(),
        }
    }

    /// Mock services holding one GitHub workflow, and that workflow's id.
    ///
    /// Both sections are passed as JSON rather than as their own types so that a
    /// test can say exactly which keys the stored configuration carries — which
    /// is the difference between "switched off" and "written before this was
    /// configurable", and those have to behave the same way.
    async fn services_with(
        _legacy_secret: &str,
        auto_merge: serde_json::Value,
        attention: serde_json::Value,
    ) -> (
        crate::services::ServicesContainer<crate::db::TenantDb>,
        automate_api::WorkflowId,
    ) {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .expect("build mock services");

        let workflow = WorkflowStore::new(&services)
            .with_index(&services)
            .create(WorkflowDraft {
                type_id: "github".into(),
                config: serde_json::json!({
                    "name": "SierraSoftworks",
                    "auto_merge": auto_merge,
                    "attention": attention,
                }),
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id;

        (services, workflow)
    }

    /// Mock services holding one GitHub workflow which serves `connection`, and
    /// that workflow's id.
    ///
    /// Separate from [`services_with`] because only the tests about which
    /// installation a workflow serves care about the field, and threading it
    /// through every other call would say nothing.
    async fn services_serving(
        connection: Option<automate_api::ConnectionId>,
    ) -> (
        crate::services::ServicesContainer<crate::db::TenantDb>,
        automate_api::WorkflowId,
    ) {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .expect("build mock services");

        let workflow = WorkflowStore::new(&services)
            .with_index(&services)
            .create(WorkflowDraft {
                type_id: "github".into(),
                config: serde_json::json!({
                    "name": "SierraSoftworks",
                    "connection": connection,
                    "auto_merge": on(),
                    "attention": off(),
                }),
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id;

        (services, workflow)
    }

    /// Runs one delivery the way the consumer would.
    async fn run(
        services: &crate::services::ServicesContainer<crate::db::TenantDb>,
        delivery: &WebhookDelivery,
    ) -> Result<(), human_errors::Error> {
        GitHubWebhook
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                delivery,
            )
            .await
    }

    async fn dispatched<S: Services>(services: &S) -> usize {
        auto_merge_tasks(services).await.len()
    }

    /// The auto-merge tasks queued, with their settings, so a test can assert on
    /// what the job will actually be told rather than only that it was told.
    async fn auto_merge_tasks<S: Services>(services: &S) -> Vec<crate::jobs::GitHubAutoMergeTask> {
        services
            .queue()
            .peek::<_, crate::jobs::GitHubAutoMergeTask>(GitHubAutoMergeWorkflow::partition(), 10)
            .await
            .expect("peek the auto-merge queue")
            .into_iter()
            .map(|message| message.payload)
            .collect()
    }

    async fn attention_tasks<S: Services>(services: &S) -> Vec<crate::jobs::GitHubAttentionTask> {
        services
            .queue()
            .peek::<_, crate::jobs::GitHubAttentionTask>(GitHubAttentionWorkflow::partition(), 10)
            .await
            .expect("peek the attention queue")
            .into_iter()
            .map(|message| message.payload)
            .collect()
    }

    async fn refreshes<S: Services>(services: &S) -> usize {
        services
            .queue()
            .peek::<_, serde_json::Value>(GitHubNotificationsRefreshWorkflow::partition(), 10)
            .await
            .expect("peek the notifications refresh queue")
            .len()
    }

    #[test]
    fn verify_signature_accepts_a_valid_signature() {
        GitHubWebhook::verify_signature("secret", BODY, &sign("secret", BODY))
            .expect("a valid signature should verify");
    }

    #[test]
    fn verify_signature_rejects_a_tampered_body() {
        let signature = sign("secret", BODY);
        assert!(GitHubWebhook::verify_signature("secret", "{}", &signature).is_err());
    }

    #[test]
    fn verify_signature_rejects_the_wrong_secret() {
        let signature = sign("other-secret", BODY);
        assert!(GitHubWebhook::verify_signature("secret", BODY, &signature).is_err());
    }

    #[test]
    fn verify_signature_rejects_an_unprefixed_signature() {
        assert!(GitHubWebhook::verify_signature("secret", BODY, "deadbeef").is_err());
    }

    #[tokio::test]
    async fn pull_request_events_are_dispatched_to_the_auto_merge_queue() {
        let (services, workflow) = services_with("secret", on(), on()).await;
        let job = event(
            workflow,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-GitHub-Delivery", "delivery-1"),
                ("X-Hub-Signature-256", &sign("secret", BODY)),
            ],
        );

        run(&services, &job)
            .await
            .expect("a signed pull_request delivery should be dispatched");

        assert_eq!(dispatched(&services).await, 1);
        assert_eq!(
            refreshes(&services).await,
            1,
            "a pull_request also moves the notifications inbox"
        );
    }

    #[test]
    fn issue_comments_normalise_onto_their_thread() {
        let job = payload(serde_json::json!({
            "action": "created",
            "issue": {
                "number": 7,
                "title": "Fix the thing",
                "html_url": "https://github.com/example/repo/issues/7",
                "user": { "login": "notheotherben" },
            },
            "comment": {
                "body": "Any update on this?",
                "html_url": "https://github.com/example/repo/issues/7#issuecomment-1",
                "user": { "login": "someone-else" },
            },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "owner": { "login": "example" },
            },
            "sender": { "login": "someone-else" },
        }));

        let parsed = GitHubAttentionEvent::parse("issue_comment", &job)
            .expect("the comment should parse")
            .expect("the comment should warrant attention");

        assert_eq!(parsed.kind, GitHubAttentionKind::Comment);
        assert_eq!(parsed.number, 7);
        assert_eq!(parsed.actor.as_deref(), Some("someone-else"));
        assert_eq!(parsed.subject_author.as_deref(), Some("notheotherben"));
        assert!(!parsed.resolved);
        assert_eq!(parsed.unique_key(), "github/attention/example/repo#7");
    }

    #[test]
    fn edited_comments_do_not_warrant_attention() {
        let job = payload(serde_json::json!({
            "action": "edited",
            "issue": { "number": 7, "title": "Fix the thing", "html_url": "https://example.com" },
            "comment": { "body": "Any update on this?" },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "owner": { "login": "example" },
            },
        }));

        assert!(
            GitHubAttentionEvent::parse("issue_comment", &job)
                .expect("the comment should parse")
                .is_none()
        );
    }

    #[test]
    fn unassignment_resolves_the_thread() {
        let job = payload(serde_json::json!({
            "action": "unassigned",
            "pull_request": {
                "number": 3,
                "title": "Bump serde",
                "html_url": "https://github.com/example/repo/pull/3",
                "user": { "login": "dependabot[bot]" },
            },
            "assignee": { "login": "notheotherben" },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "owner": { "login": "example" },
            },
            "sender": { "login": "someone-else" },
        }));

        let parsed = GitHubAttentionEvent::parse("pull_request", &job)
            .expect("the assignment should parse")
            .expect("the assignment should warrant attention");

        assert_eq!(parsed.kind, GitHubAttentionKind::Assignment);
        assert_eq!(parsed.assignee.as_deref(), Some("notheotherben"));
        assert!(parsed.resolved);
    }

    #[rstest]
    #[case("issues", "issue")]
    #[case("pull_request", "pull_request")]
    fn closing_a_subject_resolves_it(#[case] event_type: &str, #[case] field: &str) {
        let job = payload(serde_json::json!({
            "action": "closed",
            field: {
                "number": 7,
                "title": "Fix the thing",
                "html_url": "https://github.com/example/repo/issues/7",
                "user": { "login": "notheotherben" },
                "merged": true,
            },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "owner": { "login": "example" },
            },
            "sender": { "login": "someone-else" },
        }));

        let parsed = GitHubAttentionEvent::parse(event_type, &job)
            .expect("the closure should parse")
            .expect("the closure should warrant attention");

        assert_eq!(parsed.kind, GitHubAttentionKind::Closure);
        assert!(parsed.resolved);
        // The task the comment and notification paths file is keyed on the
        // subject, so this is what has to be completed.
        assert_eq!(parsed.unique_key(), "github/attention/example/repo#7");
    }

    #[test]
    fn reopening_a_subject_raises_nothing() {
        let job = payload(serde_json::json!({
            "action": "reopened",
            "issue": {
                "number": 7,
                "title": "Fix the thing",
                "html_url": "https://github.com/example/repo/issues/7",
            },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "owner": { "login": "example" },
            },
        }));

        assert!(
            GitHubAttentionEvent::parse("issues", &job)
                .expect("the event should parse")
                .is_none()
        );
    }

    #[test]
    fn dependabot_alerts_describe_the_vulnerable_package() {
        let job = payload(serde_json::json!({
            "action": "created",
            "alert": {
                "number": 12,
                "html_url": "https://github.com/example/repo/security/dependabot/12",
                "dependency": { "package": { "ecosystem": "cargo", "name": "openssl" } },
                "security_advisory": {
                    "summary": "Denial of service",
                    "description": "A malformed certificate can crash the parser.",
                    "severity": "high",
                },
            },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "owner": { "login": "example" },
            },
        }));

        let parsed = GitHubAttentionEvent::parse("dependabot_alert", &job)
            .expect("the alert should parse")
            .expect("the alert should warrant attention");

        assert_eq!(parsed.kind, GitHubAttentionKind::SecurityAlert);
        assert_eq!(parsed.title, "Denial of service in openssl");
        assert_eq!(parsed.severity.as_deref(), Some("high"));
        assert!(!parsed.resolved);
        assert_eq!(
            parsed.unique_key(),
            "github/attention/example/repo/dependabot_alert/12"
        );
    }

    #[test]
    fn secret_scanning_alerts_describe_the_leaked_secret() {
        let job = payload(serde_json::json!({
            "action": "resolved",
            "alert": {
                "number": 4,
                "html_url": "https://github.com/example/repo/security/secret-scanning/4",
                "secret_type": "aws_access_key_id",
                "secret_type_display_name": "Amazon AWS Access Key ID",
            },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "owner": { "login": "example" },
            },
        }));

        let parsed = GitHubAttentionEvent::parse("secret_scanning_alert", &job)
            .expect("the alert should parse")
            .expect("the alert should warrant attention");

        assert_eq!(parsed.title, "Leaked Amazon AWS Access Key ID");
        assert!(parsed.resolved);
    }

    #[test]
    fn code_scanning_alerts_describe_the_rule() {
        let job = payload(serde_json::json!({
            "action": "created",
            "alert": {
                "number": 9,
                "html_url": "https://github.com/example/repo/security/code-scanning/9",
                "rule": {
                    "id": "rust/sql-injection",
                    "description": "Query built from user-controlled sources",
                    "severity": "error",
                },
            },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "owner": { "login": "example" },
            },
        }));

        let parsed = GitHubAttentionEvent::parse("code_scanning_alert", &job)
            .expect("the alert should parse")
            .expect("the alert should warrant attention");

        assert_eq!(parsed.title, "Query built from user-controlled sources");
        assert_eq!(parsed.severity.as_deref(), Some("error"));
    }

    #[tokio::test]
    async fn notification_events_schedule_a_refresh() {
        let (services, workflow) = services_with("secret", off(), on()).await;
        let job = event(
            workflow,
            &[
                ("X-GitHub-Event", "issue_comment"),
                ("X-Hub-Signature-256", &sign("secret", BODY)),
            ],
        );

        run(&services, &job)
            .await
            .expect("a signed issue_comment delivery should schedule a refresh");

        assert_eq!(refreshes(&services).await, 1);
    }

    #[tokio::test]
    async fn a_matching_pull_request_is_left_alone_while_auto_merge_is_switched_off() {
        // [`BODY`] is a newly opened Dependabot pull request, which is precisely
        // what the default filter selects — so the only thing that can be
        // stopping this is the switch, which is the point of having one.
        let (services, workflow) = services_with("secret", off(), on()).await;
        let job = event(
            workflow,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-Hub-Signature-256", &sign("secret", BODY)),
            ],
        );

        run(&services, &job)
            .await
            .expect("a switched-off section should be skipped without erroring");

        assert_eq!(dispatched(&services).await, 0);
    }

    #[tokio::test]
    async fn a_section_written_before_the_switch_existed_is_off() {
        // Somebody's stored configuration says `auto_merge = {}`, which used to
        // mean "on" because presence was the switch. It has to read as off now,
        // or an upgrade would hand a capability to people who never asked for
        // it — the same reading an omitted section gets.
        let (services, workflow) =
            services_with("secret", serde_json::json!({}), serde_json::json!({})).await;
        let job = event(
            workflow,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-Hub-Signature-256", &sign("secret", BODY)),
            ],
        );

        run(&services, &job).await.expect("the delivery should run");

        assert_eq!(dispatched(&services).await, 0);
        assert!(attention_tasks(&services).await.is_empty());
    }

    #[tokio::test]
    async fn switching_auto_merge_on_dispatches_a_task_carrying_this_workflows_filter() {
        // The settings travel with the dispatch, so the thing worth asserting is
        // not that a task was queued but that it was queued with what *this*
        // workflow was configured with rather than a compiled-in default.
        let (services, workflow) = services_with(
            "secret",
            serde_json::json!({ "enabled": true, "filter": r#"repository == "example/repo""# }),
            off(),
        )
        .await;
        let job = event(
            workflow,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-Hub-Signature-256", &sign("secret", BODY)),
            ],
        );

        run(&services, &job)
            .await
            .expect("a switched-on section should dispatch");

        let tasks = auto_merge_tasks(&services).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].config.filter.raw(),
            r#"repository == "example/repo""#
        );
        assert_ne!(
            tasks[0].config.filter.raw(),
            crate::jobs::DEFAULT_AUTO_MERGE_FILTER,
            "the workflow's own filter should reach the job, not the default it would have had",
        );
    }

    #[tokio::test]
    async fn the_workflow_tells_the_job_which_installation_it_serves() {
        // The connection is what says which installation this workflow handles
        // deliveries for, and the job is where that gets checked and the token
        // minted from it — so it has to travel with the dispatch. Until it did,
        // the field was stored, asked for on the form, and read by nothing.
        let connection = automate_api::ConnectionId::from_entropy(0xC0FFEE);
        let (services, workflow) = services_serving(Some(connection)).await;
        let job = event(
            workflow,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-Hub-Signature-256", &sign("secret", BODY)),
            ],
        );

        run(&services, &job)
            .await
            .expect("a signed pull_request delivery should be dispatched");

        let tasks = auto_merge_tasks(&services).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].connection, Some(connection));
    }

    #[tokio::test]
    async fn a_workflow_naming_no_installation_dispatches_without_one() {
        // A workflow stored before the field existed still has to work, and the
        // job's fallback to the delivery's own claim can only happen if the
        // absence reaches it rather than being turned into some default.
        let (services, workflow) = services_serving(None).await;
        let job = event(
            workflow,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-Hub-Signature-256", &sign("secret", BODY)),
            ],
        );

        run(&services, &job)
            .await
            .expect("a signed pull_request delivery should be dispatched");

        let tasks = auto_merge_tasks(&services).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].connection, None);
    }

    #[tokio::test]
    async fn a_comment_is_left_alone_while_attention_is_switched_off() {
        let (services, workflow) = services_with("secret", off(), off()).await;
        let job = event_of(
            workflow,
            COMMENT_BODY,
            &[
                ("X-GitHub-Event", "issue_comment"),
                ("X-Hub-Signature-256", &sign("secret", COMMENT_BODY)),
            ],
        );

        run(&services, &job)
            .await
            .expect("a switched-off section should be skipped without erroring");

        assert!(attention_tasks(&services).await.is_empty());
        assert_eq!(
            refreshes(&services).await,
            1,
            "the notifications inbox still moved, which is not what the switch governs",
        );
    }

    #[tokio::test]
    async fn switching_attention_on_dispatches_a_task_carrying_this_workflows_filters() {
        let (services, workflow) = services_with(
            "secret",
            off(),
            serde_json::json!({ "enabled": true, "comments": r#"author == "someone-else""# }),
        )
        .await;
        let job = event_of(
            workflow,
            COMMENT_BODY,
            &[
                ("X-GitHub-Event", "issue_comment"),
                ("X-Hub-Signature-256", &sign("secret", COMMENT_BODY)),
            ],
        );

        run(&services, &job)
            .await
            .expect("a switched-on section should dispatch");

        let tasks = attention_tasks(&services).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].config.comments.raw(),
            r#"author == "someone-else""#
        );
        assert_ne!(
            tasks[0].config.comments.raw(),
            crate::jobs::DEFAULT_COMMENT_FILTER,
            "the workflow's own filter should reach the job, not the default it would have had",
        );
    }

    #[tokio::test]
    async fn unsupported_events_are_ignored() {
        let (services, workflow) = services_with("secret", on(), on()).await;
        let job = event(
            workflow,
            &[
                ("X-GitHub-Event", "check_suite"),
                ("X-Hub-Signature-256", &sign("secret", BODY)),
            ],
        );

        run(&services, &job)
            .await
            .expect("an unsupported event should be ignored without erroring");

        assert_eq!(dispatched(&services).await, 0);
        assert_eq!(refreshes(&services).await, 0);
    }

    #[tokio::test]
    async fn a_delivery_for_a_paused_workflow_does_nothing() {
        // Pausing exists so somebody can stop a busy organisation's deliveries
        // without losing their configuration, which only works if a paused
        // workflow is quiet.
        let (services, workflow) = services_with("secret", on(), on()).await;

        WorkflowStore::new(&services)
            .with_index(&services)
            .update(
                workflow,
                WorkflowDraft {
                    type_id: "github".into(),
                    config: serde_json::json!({
                        "name": "SierraSoftworks",
                        "secret": "secret",
                        "auto_merge": on(),
                        "attention": on(),
                    }),
                    schedule: None,
                    enabled: false,
                },
            )
            .await
            .unwrap();

        let job = event(
            workflow,
            &[
                ("X-GitHub-Event", "pull_request"),
                ("X-Hub-Signature-256", &sign("secret", BODY)),
            ],
        );

        run(&services, &job).await.unwrap();

        assert_eq!(dispatched(&services).await, 0);
        assert_eq!(refreshes(&services).await, 0);
    }

    #[tokio::test]
    async fn installation_events_maintain_the_connection() {
        let (services, workflow) = services_with("secret", off(), on()).await;

        let body = |action: &str| {
            serde_json::json!({
                "action": action,
                "installation": {
                    "id": 42,
                    "account": { "login": "notheotherben", "type": "User" },
                },
            })
            .to_string()
        };

        let deliver =
            async |body: String,
                   services: &crate::services::ServicesContainer<crate::db::TenantDb>| {
                let job = WebhookDelivery {
                    workflow,
                    event: WebhookEvent {
                        body: body.clone(),
                        query: String::new(),
                        headers: [
                            ("X-GitHub-Event".to_string(), "installation".to_string()),
                            ("X-Hub-Signature-256".to_string(), sign("secret", &body)),
                        ]
                        .into_iter()
                        .collect::<HashMap<_, _>>(),
                    },
                };

                run(services, &job)
                    .await
                    .expect("the installation event should be handled");
            };

        let linked = async |services: &crate::services::ServicesContainer<crate::db::TenantDb>| {
            crate::connections::ConnectionStore::for_services(services)
                .list_for_provider(crate::integrations::github_app::GITHUB_PROVIDER)
                .await
                .expect("list the linked GitHub accounts")
        };

        deliver(body("created"), &services).await;

        let recorded = linked(&services).await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].account.as_deref(), Some("notheotherben"));
        // The account type rides on the connection now; there is no second
        // record left for it to live in.
        assert_eq!(
            recorded[0]
                .metadata
                .get(crate::integrations::github_app::ACCOUNT_TYPE)
                .and_then(serde_json::Value::as_str),
            Some("User")
        );

        deliver(body("deleted"), &services).await;

        assert!(
            linked(&services).await.is_empty(),
            "uninstalling should forget the account"
        );
    }

    #[test]
    fn a_stored_configuration_names_the_workflow_it_describes() {
        let workflow = crate::workflows::lookup("github").expect("the type is registered");

        assert_eq!(
            workflow
                .describe(&serde_json::json!({ "name": "SierraSoftworks" }))
                .unwrap(),
            "SierraSoftworks",
        );
    }

    /// The value a form would be prefilled with for `path`.
    fn descriptor_default(path: &str) -> serde_json::Value {
        use crate::workflows::ConfigurableWorkflow;

        GitHubWebhook::descriptor()
            .fields
            .into_iter()
            .find(|field| field.name == path)
            .unwrap_or_else(|| panic!("the descriptor should describe '{path}'"))
            .default
            .unwrap_or_else(|| panic!("'{path}' should offer a default"))
    }

    #[test]
    fn an_untouched_form_says_the_same_thing_as_an_omitted_section() {
        // A new form starts at the descriptor's defaults; a stored configuration
        // that leaves a section out starts at the struct's `serde(default)`.
        // Those are two copies of the same intention, and if they drift then
        // "I left it alone" and "I never had it" quietly come to mean different
        // things — which nobody notices until a filter stops matching.
        let omitted: GitHubWebhookConfig =
            serde_json::from_value(serde_json::json!({ "name": "SierraSoftworks" }))
                .expect("a configuration naming only the workflow should load");

        for (path, from_the_struct) in [
            (
                crate::config_path!(GitHubWebhookConfig: auto_merge.enabled),
                serde_json::to_value(omitted.auto_merge.enabled).unwrap(),
            ),
            (
                crate::config_path!(GitHubWebhookConfig: auto_merge.filter),
                serde_json::to_value(&omitted.auto_merge.filter).unwrap(),
            ),
            (
                crate::config_path!(GitHubWebhookConfig: auto_merge.approve),
                serde_json::to_value(omitted.auto_merge.approve).unwrap(),
            ),
            (
                crate::config_path!(GitHubWebhookConfig: auto_merge.approval_message),
                serde_json::to_value(&omitted.auto_merge.approval_message).unwrap(),
            ),
            (
                crate::config_path!(GitHubWebhookConfig: attention.enabled),
                serde_json::to_value(omitted.attention.enabled).unwrap(),
            ),
            (
                crate::config_path!(GitHubWebhookConfig: attention.comments),
                serde_json::to_value(&omitted.attention.comments).unwrap(),
            ),
            (
                crate::config_path!(GitHubWebhookConfig: attention.assignments),
                serde_json::to_value(&omitted.attention.assignments).unwrap(),
            ),
            (
                crate::config_path!(GitHubWebhookConfig: attention.security_alerts),
                serde_json::to_value(&omitted.attention.security_alerts).unwrap(),
            ),
        ] {
            assert_eq!(
                descriptor_default(path),
                from_the_struct,
                "the form and the configuration disagree about what '{path}' starts as",
            );
        }
    }

    #[test]
    fn both_sections_start_switched_off() {
        // The switches are what make these settings reachable by a dotted path,
        // but they are also a promise: nobody acquires the ability to merge
        // their own pull requests, or a stream of Todoist tasks, by upgrading.
        let omitted: GitHubWebhookConfig =
            serde_json::from_value(serde_json::json!({ "name": "SierraSoftworks" })).unwrap();

        assert!(!omitted.auto_merge.enabled);
        assert!(!omitted.attention.enabled);
        assert_eq!(descriptor_default("auto_merge.enabled"), false);
        assert_eq!(descriptor_default("attention.enabled"), false);
    }

    #[test]
    fn the_form_asks_for_everything_these_deliveries_are_treated_by() {
        // Listed rather than counted, so that dropping a setting off the form -
        // which is how these came to be unconfigurable in the first place - is a
        // decision somebody makes here instead of a number quietly going down.
        use crate::workflows::ConfigurableWorkflow;

        let names: Vec<String> = GitHubWebhook::descriptor()
            .fields
            .into_iter()
            .map(|field| field.name)
            .collect();

        assert_eq!(
            names,
            vec![
                "name",
                "connection",
                "auto_merge.enabled",
                "auto_merge.filter",
                "auto_merge.approve",
                "auto_merge.approval_message",
                "attention.enabled",
                "attention.comments",
                "attention.assignments",
                "attention.security_alerts",
            ],
        );
    }

    #[test]
    fn the_installation_is_the_only_thing_besides_a_name_a_workflow_must_be_told() {
        // Which installation a workflow serves cannot be inferred from a
        // delivery we have not yet decided to trust, and installing the App is
        // now what puts an account in the picker, so insisting on it is a
        // question somebody can actually answer. Everything else has a sensible
        // starting point.
        use crate::workflows::ConfigurableWorkflow;

        let required: Vec<String> = GitHubWebhook::descriptor()
            .fields
            .into_iter()
            .filter(|field| field.required)
            .map(|field| field.name)
            .collect();

        assert_eq!(required, vec!["name", "connection"]);
    }

    #[test]
    fn the_filter_editor_offers_the_names_the_events_actually_answer_to() {
        // A suggested field the `Filterable` impl does not know about evaluates
        // to null, so the filter silently never matches; the editor's list and
        // the impl have to be the same list.
        let pull_request: GitHubPullRequestEvent =
            serde_json::from_str(BODY).expect("the sample pull request should load");

        for name in GitHubWebhook::pull_request_filter_fields() {
            assert!(
                !matches!(pull_request.get(&name), crate::filter::FilterValue::Null),
                "the editor suggests '{name}', which a pull request does not answer to",
            );
        }

        let attention = GitHubAttentionEvent::parse(
            "issue_comment",
            &payload(serde_json::from_str(COMMENT_BODY).expect("the sample comment should load")),
        )
        .expect("the comment should parse")
        .expect("the comment should warrant attention");

        // `assignee` and `severity` are null on a comment by nature - they only
        // exist on an assignment or an alert - so they are excused here rather
        // than dropped from the editor's suggestions.
        for name in GitHubWebhook::attention_filter_fields() {
            if matches!(name.as_str(), "assignee" | "severity") {
                continue;
            }

            assert!(
                !matches!(attention.get(&name), crate::filter::FilterValue::Null),
                "the editor suggests '{name}', which an attention event does not answer to",
            );
        }
    }
}
