use hmac::{Hmac, Mac};
use serde::Deserialize;
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
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).wrap_err_as_user(
            "Failed to create HMAC instance with the provided secret.",
            &[
                "Ensure that you have provided a valid webhooks.sentry.secret in your configuration.",
                "Ensure that the configured webhooks.sentry.secret matches the client secret in your Sentry integration settings.",
            ],
        )?;

        // Compute the HMAC of the body
        mac.update(body.as_bytes());

        // Decode the expected signature from hex
        let expected_signature = hex::decode(signature).map_err_as_user(&[
            "The signature in the Sentry-Hook-Signature header is not valid hex.",
            "Ensure that you are only sending Sentry webhooks to this endpoint.",
        ])?;

        // Verify the signature
        mac.verify_slice(&expected_signature).wrap_err_as_user(
            "Webhook signature verification failed (signatures did not match).".to_string(),
            &[
                "Ensure that the configured webhooks.sentry.secret matches the client secret in your Sentry integration settings.",
                "Check that you have configured the webhook correctly in Sentry.",
            ],
        )?;

        Ok(())
    }
}

impl Job for SentryAlertsWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/sentry"
    }

    #[instrument("webhooks.sentry.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
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

        let notification: SentryIssueNotification = job.json()?;

        // Only process created issues (new errors)
        if !notification.action.eq_ignore_ascii_case("created") {
            info!("Ignoring non-created Sentry issue: {}", notification.action);
            return Ok(());
        }

        if !services
            .config()
            .webhooks
            .sentry
            .filter
            .matches(&notification)?
        {
            info!(
                "Sentry issue '{}' did not match filter; ignoring.",
                notification.data.issue.title
            );
            return Ok(());
        }

        let issue = &notification.data.issue;
        let description = if !issue.culprit.is_empty() {
            Some(format!(
                "{}\n\nProject: {}",
                issue.culprit, issue.project.name
            ))
        } else {
            Some(format!("Project: {}", issue.project.name))
        };

        let priority = match issue.level.as_str() {
            "fatal" => 4,
            "error" => 4,
            "warning" => 3,
            "info" => 2,
            "debug" => 1,
            _ => 3,
        };

        TodoistCreateTask::dispatch(
            TodoistCreateTaskPayload {
                title: format!(
                    "[**Sentry {}**]({}): {}",
                    issue._type.to_uppercase(),
                    issue.web_url,
                    issue.title
                ),
                description,
                due: TodoistDueDate::DateTime(chrono::Utc::now()),
                priority: Some(priority),
                config: services.config().webhooks.sentry.todoist.clone(),
                ..Default::default()
            },
            None,
            &services,
        )
        .await?;

        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct SentryIssueNotification {
    action: String,
    actor: SentryActor,
    data: SentryIssueData,
}

impl Filterable for SentryIssueNotification {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "action" => self.action.clone().into(),
            "issue_id" => self.data.issue.id.clone().into(),
            "issue_title" => self.data.issue.title.clone().into(),
            "issue_type" => self.data.issue._type.clone().into(),
            "issue_level" => self.data.issue.level.clone().into(),
            "project_name" => self.data.issue.project.name.clone().into(),
            "project_platform" => self.data.issue.project.platform.clone().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct SentryActor {
    #[serde(rename = "type")]
    _type: String,
    id: String,
    name: String,
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
    _type: String,
    level: String,
    #[serde(rename = "shortId")]
    short_id: String,
    culprit: String,
    project: SentryProject,
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
}
