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

/// Alert and assignment actions which mean the subject no longer needs
/// attention, so any task tracking it is completed instead of raised.
const RESOLVING_ACTIONS: &[&str] = &[
    "auto_dismissed",
    "closed_by_user",
    "dismissed",
    "fixed",
    "resolved",
    "unassigned",
];

#[derive(Clone, Deserialize, Default)]
pub struct GitHubWebhookConfig {
    /// The shared secret configured on the GitHub webhook, used to verify the
    /// `X-Hub-Signature-256` HMAC. Deliveries are rejected when this is unset,
    /// because the events it carries drive writes to your repositories.
    #[serde(default)]
    pub secret: String,

    /// Enables auto-merge handling for `pull_request` events when present.
    #[serde(default)]
    pub auto_merge: Option<GitHubAutoMergeConfig>,

    /// Enables Todoist reminders for comments, assignments and security alerts
    /// when present.
    #[serde(default)]
    pub attention: Option<GitHubAttentionConfig>,
}

/// The entry point for GitHub's organization-level webhook.
///
/// GitHub delivers every event type to a single endpoint, so this job performs
/// the work which is common to all of them - signature verification and event
/// type routing - and then hands the parsed payload to the dedicated job queue
/// which knows how to act on it.
#[derive(Clone)]
pub struct GitHubWebhook;

impl GitHubWebhook {
    /// Verifies the `X-Hub-Signature-256` header, which GitHub populates with
    /// `sha256=<hex>` where the digest is an HMAC-SHA256 of the raw request
    /// body keyed with the webhook secret.
    ///
    /// See https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
    fn verify_signature(
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
            &["Ensure that you have provided a valid webhooks.github.secret in your configuration."],
        )?;

        mac.update(body.as_bytes());

        mac.verify_slice(&expected_signature).wrap_user_err(
            "Webhook signature verification failed (signatures did not match).".to_string(),
            &[
                "Ensure that the configured webhooks.github.secret matches the secret set on the webhook in GitHub.",
            ],
        )?;

        Ok(())
    }

    fn header<'a>(job: &'a WebhookEvent, name: &str) -> Option<&'a str> {
        job.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    async fn track_installation(
        job: &WebhookEvent,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        let event: GitHubInstallationEvent = job.json()?;
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

impl Job for GitHubWebhook {
    type JobType = WebhookEvent;

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
        let config = services.config();
        let github = &config.webhooks.github;

        // Unlike the observability webhooks, GitHub deliveries cause us to write
        // to repositories, so an unsigned delivery is rejected rather than
        // trusted.
        if github.secret.is_empty() {
            warn!(
                "Received a GitHub webhook but no webhooks.github.secret is configured; rejecting request."
            );
            return Ok(());
        }

        let Some(signature) = Self::header(job, "x-hub-signature-256") else {
            warn!(
                "Received a GitHub webhook without an X-Hub-Signature-256 header; rejecting request."
            );
            return Ok(());
        };

        if let Err(err) = Self::verify_signature(&github.secret, &job.body, signature) {
            warn!(
                "Failed to verify GitHub webhook signature, rejecting request: {}",
                err
            );
            return Ok(());
        }

        // GitHub identifies each delivery with a GUID which is preserved across
        // its own retries, making it a natural idempotency key for the jobs we
        // fan out to.
        let delivery = Self::header(job, "x-github-delivery").map(|id| Cow::Owned(id.to_string()));
        let event_type = Self::header(job, "x-github-event").unwrap_or_default();

        if event_type == "ping" {
            info!("Received a GitHub webhook ping event.");
            return Ok(());
        }

        // GitHub reports installs and uninstalls directly, which keeps the
        // registry accurate even when someone installs the App from GitHub
        // rather than through the wizard.
        if event_type == "installation" {
            return Self::track_installation(job, services).await;
        }

        let mut handled = false;

        if event_type == "pull_request" && github.auto_merge.is_some() {
            let event: GitHubPullRequestEvent = job.json()?;
            GitHubAutoMergeWorkflow::dispatch(event, delivery.clone(), services).await?;
            handled = true;
        }

        if github.attention.is_some() {
            // A payload we cannot interpret is skipped rather than raised, so that
            // an unmodelled variant cannot poison the queue by retrying forever.
            match GitHubAttentionEvent::parse(event_type, job) {
                Ok(Some(event)) => {
                    GitHubAttentionWorkflow::dispatch(event, delivery, services).await?;
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
}

impl GitHubAttentionKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Assignment => "assignment",
            Self::SecurityAlert => "security_alert",
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

            if !matches!(payload.action.as_str(), "assigned" | "unassigned") {
                return Ok(None);
            }

            let Some(subject) = payload.subject() else {
                return Ok(None);
            };

            return Ok(Some(Self {
                kind: GitHubAttentionKind::Assignment,
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
    use std::collections::HashMap;

    const BODY: &str = r#"{"action":"opened","number":1,"pull_request":{"node_id":"PR_node","number":1,"html_url":"https://github.com/example/repo/pull/1","title":"Bump serde","draft":false,"user":{"login":"dependabot[bot]"}},"repository":{"name":"repo","full_name":"example/repo","private":false,"owner":{"login":"example"}},"sender":{"login":"dependabot[bot]"}}"#;

    fn sign(secret: &str, body: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn event(headers: &[(&str, &str)]) -> WebhookEvent {
        WebhookEvent {
            body: BODY.to_string(),
            query: String::new(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        }
    }

    fn delivery(body: serde_json::Value) -> WebhookEvent {
        WebhookEvent {
            body: body.to_string(),
            query: String::new(),
            headers: HashMap::new(),
        }
    }

    async fn services_with(
        secret: &str,
        auto_merge: Option<GitHubAutoMergeConfig>,
    ) -> crate::services::ServicesContainer<crate::db::TenantDb> {
        let secret = secret.to_string();
        crate::services::ServicesContainer::new_custom_mock(move |config, _| {
            config.webhooks.github = GitHubWebhookConfig {
                secret,
                auto_merge,
                attention: Some(GitHubAttentionConfig::default()),
            };
        })
        .await
        .expect("build mock services")
    }

    async fn dispatched<S: Services>(services: &S) -> usize {
        services
            .queue()
            .peek::<_, serde_json::Value>(GitHubAutoMergeWorkflow::partition(), 10)
            .await
            .expect("peek the auto-merge queue")
            .len()
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
    async fn unsigned_deliveries_are_rejected() {
        let services = services_with("secret", Some(GitHubAutoMergeConfig::default())).await;
        let job = event(&[("X-GitHub-Event", "pull_request")]);

        GitHubWebhook
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &job,
            )
            .await
            .expect("an unsigned delivery should be rejected without erroring");

        assert_eq!(dispatched(&services).await, 0);
    }

    #[tokio::test]
    async fn pull_request_events_are_dispatched_to_the_auto_merge_queue() {
        let services = services_with("secret", Some(GitHubAutoMergeConfig::default())).await;
        let job = event(&[
            ("X-GitHub-Event", "pull_request"),
            ("X-GitHub-Delivery", "delivery-1"),
            ("X-Hub-Signature-256", &sign("secret", BODY)),
        ]);

        GitHubWebhook
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &job,
            )
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
        let job = delivery(serde_json::json!({
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
        let job = delivery(serde_json::json!({
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
        let job = delivery(serde_json::json!({
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

    #[test]
    fn dependabot_alerts_describe_the_vulnerable_package() {
        let job = delivery(serde_json::json!({
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
        let job = delivery(serde_json::json!({
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
        let job = delivery(serde_json::json!({
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
        let services = services_with("secret", None).await;
        let job = event(&[
            ("X-GitHub-Event", "issue_comment"),
            ("X-Hub-Signature-256", &sign("secret", BODY)),
        ]);

        GitHubWebhook
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &job,
            )
            .await
            .expect("a signed issue_comment delivery should schedule a refresh");

        assert_eq!(refreshes(&services).await, 1);
    }

    #[tokio::test]
    async fn pull_request_events_are_ignored_when_auto_merge_is_not_configured() {
        let services = services_with("secret", None).await;
        let job = event(&[
            ("X-GitHub-Event", "pull_request"),
            ("X-Hub-Signature-256", &sign("secret", BODY)),
        ]);

        GitHubWebhook
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &job,
            )
            .await
            .expect("an unconfigured event should be ignored without erroring");

        assert_eq!(dispatched(&services).await, 0);
    }

    #[tokio::test]
    async fn unsupported_events_are_ignored() {
        let services = services_with("secret", Some(GitHubAutoMergeConfig::default())).await;
        let job = event(&[
            ("X-GitHub-Event", "check_suite"),
            ("X-Hub-Signature-256", &sign("secret", BODY)),
        ]);

        GitHubWebhook
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &job,
            )
            .await
            .expect("an unsupported event should be ignored without erroring");

        assert_eq!(dispatched(&services).await, 0);
        assert_eq!(refreshes(&services).await, 0);
    }

    #[tokio::test]
    async fn installation_events_maintain_the_registry() {
        let services = services_with("secret", None).await;

        let payload = |action: &str| {
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
                let job = WebhookEvent {
                    body: body.clone(),
                    query: String::new(),
                    headers: [
                        ("X-GitHub-Event".to_string(), "installation".to_string()),
                        ("X-Hub-Signature-256".to_string(), sign("secret", &body)),
                    ]
                    .into_iter()
                    .collect::<HashMap<_, _>>(),
                };

                GitHubWebhook
                    .handle(
                        JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                        &job,
                    )
                    .await
                    .expect("the installation event should be handled");
            };

        deliver(payload("created"), &services).await;

        let recorded = services
            .kv()
            .get::<crate::services::GitHubInstallation>(
                crate::integrations::github_app::INSTALLATIONS_PARTITION,
                "notheotherben",
            )
            .await
            .expect("read the registry")
            .expect("the installation should be recorded");
        assert_eq!(recorded.id, 42);
        assert_eq!(recorded.account_type, "User");

        deliver(payload("deleted"), &services).await;

        assert!(
            services
                .kv()
                .get::<crate::services::GitHubInstallation>(
                    crate::integrations::github_app::INSTALLATIONS_PARTITION,
                    "notheotherben",
                )
                .await
                .expect("read the registry")
                .is_none(),
            "uninstalling should forget the account"
        );
    }
}
