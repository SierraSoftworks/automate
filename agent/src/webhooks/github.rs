use std::borrow::Cow;
use std::fmt::Display;

use hmac::{Hmac, KeyInit, Mac};

use crate::jobs::{GitHubAutoMergeConfig, GitHubAutoMergeWorkflow};
use crate::prelude::*;

type HmacSha256 = Hmac<Sha256>;

use sha2::Sha256;

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

        match event_type {
            "ping" => {
                info!("Received a GitHub webhook ping event.");
                Ok(())
            }
            "pull_request" => {
                if github.auto_merge.is_none() {
                    debug!(
                        "Received a GitHub pull_request event but no webhooks.github.auto_merge configuration is present; ignoring."
                    );
                    return Ok(());
                }

                let event: GitHubPullRequestEvent = job.json()?;
                GitHubAutoMergeWorkflow::dispatch(event, delivery, services).await
            }
            other => {
                debug!("Received an unsupported GitHub webhook event '{other}'; ignoring.");
                Ok(())
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const BODY: &str = r#"{"action":"opened","number":1,"pull_request":{"node_id":"PR_node","html_url":"https://github.com/example/repo/pull/1","title":"Bump serde","draft":false,"user":{"login":"dependabot[bot]"}},"repository":{"name":"repo","full_name":"example/repo","private":false,"owner":{"login":"example"}},"sender":{"login":"dependabot[bot]"}}"#;

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

    async fn services_with(
        secret: &str,
        auto_merge: Option<GitHubAutoMergeConfig>,
    ) -> crate::services::ServicesContainer<crate::db::SqliteDatabase> {
        let secret = secret.to_string();
        crate::services::ServicesContainer::new_custom_mock(move |config, _| {
            config.webhooks.github = GitHubWebhookConfig { secret, auto_merge };
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
    }
}
