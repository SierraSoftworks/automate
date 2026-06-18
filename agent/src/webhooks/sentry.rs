use std::fmt::Display;

use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;

use crate::{
    prelude::*,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate},
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Deserialize, Default)]
pub struct SentryWebhookConfig {
    pub secret: String,

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
pub struct SentryAlertsWebhook;

impl SentryAlertsWebhook {
    /// Verifies the Sentry webhook signature.
    ///
    /// According to https://docs.sentry.io/organization/integrations/integration-platform/webhooks/,
    /// Sentry signs webhooks using HMAC-SHA256 with the client secret, and includes
    /// the signature in the Sentry-Hook-Signature header.
    fn verify_signature(
        secret: &str,
        body: &str,
        signature: &str,
    ) -> Result<(), human_errors::Error> {
        // Create HMAC-SHA256 instance with the secret
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).wrap_user_err(
            "Failed to create HMAC instance with the provided secret.",
            &[
                "Ensure that you have provided a valid webhooks.sentry.secret in your configuration.",
                "Ensure that the configured webhooks.sentry.secret matches the client secret in your Sentry integration settings.",
            ],
        )?;

        // Compute the HMAC of the body
        mac.update(body.as_bytes());

        // Decode the expected signature from hex
        let expected_signature = hex::decode(signature).or_user_err(&[
            "The signature in the Sentry-Hook-Signature header is not valid hex.",
            "Ensure that you are only sending Sentry webhooks to this endpoint.",
        ])?;

        // Verify the signature
        mac.verify_slice(&expected_signature).wrap_user_err(
            "Webhook signature verification failed (signatures did not match).".to_string(),
            &[
                "Ensure that the configured webhooks.sentry.secret matches the client secret in your Sentry integration settings.",
                "Check that you have configured the webhook correctly in Sentry.",
            ],
        )?;

        Ok(())
    }
}

crate::register_job!(SentryAlertsWebhook);

impl Job for SentryAlertsWebhook {
    type JobType = super::WebhookEvent;

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

        // Validate the Sentry webhook signature header
        // https://docs.sentry.io/organization/integrations/integration-platform/webhooks/
        let secret = &services.config().webhooks.sentry.secret;

        if !secret.is_empty() {
            // HTTP headers are case-insensitive, so we need to search for the header with case-insensitive comparison
            let signature = job
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("sentry-hook-signature"))
                .map(|(_, value)| value.as_str());

            if let Some(signature) = signature {
                Self::verify_signature(secret, &job.body, signature)?;
            } else {
                warn!(
                    "Received Sentry webhook without signature, but secret is configured; rejecting request."
                );
                return Ok(());
            }
        } else {
            debug!("No Sentry webhook secret configured; skipping signature verification.");
        }

        let notification: SentryNotification = job.json()?;

        match notification {
            SentryNotification::Integration(integration) => {
                // Only process created issues (new errors)
                if !integration.action.eq_ignore_ascii_case("created") {
                    info!("Ignoring non-created Sentry issue: {}", integration.action);
                    return Ok(());
                }

                if !services
                    .config()
                    .webhooks
                    .sentry
                    .filter
                    .matches(&integration)?
                {
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
                        config: services.config().webhooks.sentry.todoist.clone(),
                        ..Default::default()
                    },
                    None,
                    services,
                )
                .await?;
            }
            SentryNotification::Alert(alert) => {
                if !services.config().webhooks.sentry.filter.matches(&alert)? {
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
                        config: services.config().webhooks.sentry.todoist.clone(),
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
    use super::*;
    use std::collections::HashMap;

    /// Helper function to generate a valid signature for testing
    fn generate_signature(secret: &str, body: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    #[test]
    fn test_verify_signature_valid() {
        let secret = "test_secret_key";
        let body = r#"{"action":"created","actor":{"type":"application","id":"sentry","name":"Sentry"},"data":{"issue":{"id":"123","url":"https://sentry.io/api/0/issues/123/","web_url":"https://sentry.io/issues/123/","project_url":"https://sentry.io/projects/my-project/","title":"Test Error","type":"error","level":"error","shortId":"TEST-1","culprit":"test.js","project":{"id":"1","name":"Test Project","platform":"javascript","slug":"test-project"}}}}"#;
        let signature = generate_signature(secret, body);

        let result = SentryAlertsWebhook::verify_signature(secret, body, &signature);
        result.expect("Valid signature should verify successfully");
    }

    #[test]
    fn test_verify_signature_invalid() {
        let secret = "test_secret_key";
        let body = r#"{"action":"created","actor":{"type":"application","id":"sentry","name":"Sentry"},"data":{"issue":{"id":"123","url":"https://sentry.io/api/0/issues/123/","web_url":"https://sentry.io/issues/123/","project_url":"https://sentry.io/projects/my-project/","title":"Test Error","type":"error","level":"error","shortId":"TEST-1","culprit":"test.js","project":{"id":"1","name":"Test Project","platform":"javascript","slug":"test-project"}}}}"#;
        let wrong_signature = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = SentryAlertsWebhook::verify_signature(secret, body, wrong_signature);
        assert!(
            result.is_err(),
            "Invalid signature should fail verification"
        );
    }

    #[test]
    fn test_verify_signature_wrong_secret() {
        let secret = "test_secret_key";
        let wrong_secret = "wrong_secret_key";
        let body = r#"{"action":"created","actor":{"type":"application","id":"sentry","name":"Sentry"},"data":{"issue":{"id":"123","url":"https://sentry.io/api/0/issues/123/","web_url":"https://sentry.io/issues/123/","project_url":"https://sentry.io/projects/my-project/","title":"Test Error","type":"error","level":"error","shortId":"TEST-1","culprit":"test.js","project":{"id":"1","name":"Test Project","platform":"javascript","slug":"test-project"}}}}"#;
        let signature = generate_signature(wrong_secret, body);

        let result = SentryAlertsWebhook::verify_signature(secret, body, &signature);
        assert!(
            result.is_err(),
            "Signature with wrong secret should fail verification"
        );
    }

    #[test]
    fn test_verify_signature_tampered_body() {
        let secret = "test_secret_key";
        let original_body = r#"{"action":"created","actor":{"type":"application","id":"sentry","name":"Sentry"},"data":{"issue":{"id":"123","url":"https://sentry.io/api/0/issues/123/","web_url":"https://sentry.io/issues/123/","project_url":"https://sentry.io/projects/my-project/","title":"Test Error","type":"error","level":"error","shortId":"TEST-1","culprit":"test.js","project":{"id":"1","name":"Test Project","platform":"javascript","slug":"test-project"}}}}"#;
        let tampered_body = r#"{"action":"created","actor":{"type":"application","id":"sentry","name":"Sentry"},"data":{"issue":{"id":"123","url":"https://sentry.io/api/0/issues/123/","web_url":"https://sentry.io/issues/123/","project_url":"https://sentry.io/projects/my-project/","title":"Tampered Error","type":"error","level":"error","shortId":"TEST-1","culprit":"test.js","project":{"id":"1","name":"Test Project","platform":"javascript","slug":"test-project"}}}}"#;
        let signature = generate_signature(secret, original_body);

        let result = SentryAlertsWebhook::verify_signature(secret, tampered_body, &signature);
        assert!(result.is_err(), "Tampered body should fail verification");
    }

    #[test]
    fn test_verify_signature_empty_body() {
        let secret = "test_secret_key";
        let body = "";
        let signature = generate_signature(secret, body);

        let result = SentryAlertsWebhook::verify_signature(secret, body, &signature);
        result.expect("Empty body with valid signature should verify successfully");
    }

    #[test]
    fn test_header_lookup_case_insensitive() {
        // Test that header lookup works with different case variations
        let signature = "abcdef0123456789";

        // Test with lowercase
        let mut headers = HashMap::new();
        headers.insert("sentry-hook-signature".to_string(), signature.to_string());

        let found = headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("sentry-hook-signature"))
            .map(|(_, value)| value.as_str());
        assert!(found.is_some(), "Should find lowercase header");

        // Test with uppercase
        let mut headers = HashMap::new();
        headers.insert("SENTRY-HOOK-SIGNATURE".to_string(), signature.to_string());

        let found = headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("sentry-hook-signature"))
            .map(|(_, value)| value.as_str());
        assert!(found.is_some(), "Should find uppercase header");

        // Test with mixed case
        let mut headers = HashMap::new();
        headers.insert("Sentry-Hook-Signature".to_string(), signature.to_string());

        let found = headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("sentry-hook-signature"))
            .map(|(_, value)| value.as_str());
        assert!(found.is_some(), "Should find mixed case header");
    }

    #[test]
    fn test_parse_integration_webhook() {
        let body = r#"{"action":"created","actor":{"type":"application","id":"sentry","name":"Sentry"},"data":{"issue":{"id":"123","url":"https://sentry.io/api/0/issues/123/","web_url":"https://sentry.io/issues/123/","project_url":"https://sentry.io/projects/my-project/","title":"Test Error","type":"error","level":"error","shortId":"TEST-1","culprit":"test.js","project":{"id":"1","name":"Test Project","platform":"javascript","slug":"test-project"}}}}"#;
        let notification: SentryNotification = serde_json::from_str(body).unwrap();
        assert!(
            matches!(notification, SentryNotification::Integration(_)),
            "Should parse as Integration webhook"
        );
    }

    #[test]
    fn test_parse_alert_webhook() {
        let body = r#"{"id":"7470688687","project":"git-tool","project_name":"git-tool","project_slug":"git-tool","logger":"root","level":"error","culprit":"main in main","message":"","url":"https://sierra-softworks.sentry.io/issues/7470688687/","triggering_rules":["Send a notification"],"event":{"title":"Test Error: something went wrong","metadata":{"type":"Test Error","value":"something went wrong"}}}"#;
        let notification: SentryNotification = serde_json::from_str(body).unwrap();
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
