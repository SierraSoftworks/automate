use chrono::DateTime;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::{prelude::*, publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate}};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Deserialize, Default)]
pub struct TailscaleWebhookConfig {
    pub secret: String,

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
pub struct TailscaleWebhook;

impl TailscaleWebhook {
    /// Verifies the Tailscale webhook signature.
    /// 
    /// According to https://tailscale.com/kb/1213/webhooks#verifying-an-event-signature,
    /// Tailscale signs webhooks using HMAC-SHA256 with the webhook secret, and includes
    /// the signature in the X-Tailscale-Signature header as a hex-encoded string.
    fn verify_signature(secret: &str, body: &str, signature_header: &str) -> Result<(), human_errors::Error> {
        // Create HMAC-SHA256 instance with the secret
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .wrap_err_as_system(
                "Failed to create HMAC instance with the provided secret.",
                &["Ensure the webhook secret is properly configured."]
            )?;
        
        // Compute the HMAC of the request body
        mac.update(body.as_bytes());
        
        // Decode the expected signature from hex
        let expected_signature = hex::decode(signature_header)
            .wrap_err_as_user(
                "Failed to decode the X-Tailscale-Signature header as hex.",
                &["The signature should be a valid hex-encoded string."]
            )?;
        
        // Verify the signature
        mac.verify_slice(&expected_signature)
            .wrap_err_as_user(
                "Webhook signature verification failed.",
                &["The signature does not match the expected value. This could indicate a tampered request or incorrect webhook secret."]
            )?;
        
        Ok(())
    }
}

impl Job for TailscaleWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/tailscale"
    }

    async fn handle(&self, job: &Self::JobType, services: impl Services + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        // Validate the Tailscale webhook signature header
        // https://tailscale.com/kb/1213/webhooks#verifying-an-event-signature
        let secret = &services.config().webhooks.tailscale.secret;
        
        if !secret.is_empty() {
            if let Some(signature) = job.headers.get("x-tailscale-signature") {
                Self::verify_signature(secret, &job.body, signature)?;
            } else {
                warn!("Received Tailscale webhook without signature, but secret is configured; rejecting request.");
                return Ok(());
            }
        } else {
            debug!("No Tailscale webhook secret configured; skipping signature verification.");
        }

        let event: TailscaleAlertEventPayload = job.json()?;

        let pretty_payload = serde_json::to_string_pretty(&event.data) 
            .unwrap_or_else(|_| job.body.clone());
        
        TodoistCreateTask::dispatch(
            TodoistCreateTaskPayload {
                title: format!(
                    "[**Tailscale**](https://login.tailscale.com/admin): {}",
                    event.message
                ),
                description: Some(format!("```\n{pretty_payload}\n```")),
                due: TodoistDueDate::DateTime(event.timestamp),
                priority: Some(match event._type.as_str() {
                    "exitNodeIPForwardingNotEnabled" => 4,
                    "subnetIPForwardingNotEnabled" => 4,
                    "nodeNeedsApproval" => 4,
                    "nodeKeyExpired" => 4,
                    "userNeedsApproval" => 4,

                    "policyUpdate" => 3,
                    "nodeCreated" => 3,
                    "nodeApproved" => 3,
                    "nodeKeyExpiringInOneDay" => 3,
                    "userCreated" => 3,
                    "userApproved" => 3,
                    "userRoleUpdated" => 3,

                    "nodeDeleted" => 2,
                    "webhookUpdated" => 2,
                    "webhookDeleted" => 2,

                    "test" => 1,

                    _ => 3
                }),
                config: services.config().webhooks.tailscale.todoist.clone(),
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
struct TailscaleAlertEventPayload {
    version: u32,
    timestamp: DateTime<chrono::Utc>,
    #[serde(rename = "type")]
    _type: String,
    tailnet: String,
    message: String,
    data: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    /// Helper function to generate a valid signature for testing
    fn generate_signature(secret: &str, body: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    #[test]
    fn test_verify_signature_valid() {
        let secret = "test_secret_key";
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let signature = generate_signature(secret, body);

        let result = TailscaleWebhook::verify_signature(secret, body, &signature);
        assert!(result.is_ok(), "Valid signature should verify successfully");
    }

    #[test]
    fn test_verify_signature_invalid() {
        let secret = "test_secret_key";
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let wrong_signature = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = TailscaleWebhook::verify_signature(secret, body, wrong_signature);
        assert!(result.is_err(), "Invalid signature should fail verification");
    }

    #[test]
    fn test_verify_signature_wrong_secret() {
        let secret = "test_secret_key";
        let wrong_secret = "wrong_secret_key";
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let signature = generate_signature(wrong_secret, body);

        let result = TailscaleWebhook::verify_signature(secret, body, &signature);
        assert!(result.is_err(), "Signature with wrong secret should fail verification");
    }

    #[test]
    fn test_verify_signature_tampered_body() {
        let secret = "test_secret_key";
        let original_body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let tampered_body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Tampered message","data":{}}"#;
        let signature = generate_signature(secret, original_body);

        let result = TailscaleWebhook::verify_signature(secret, tampered_body, &signature);
        assert!(result.is_err(), "Tampered body should fail verification");
    }

    #[test]
    fn test_verify_signature_invalid_hex() {
        let secret = "test_secret_key";
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let invalid_hex = "not_a_valid_hex_string";

        let result = TailscaleWebhook::verify_signature(secret, body, invalid_hex);
        assert!(result.is_err(), "Invalid hex signature should fail");
    }

    #[test]
    fn test_verify_signature_empty_body() {
        let secret = "test_secret_key";
        let body = "";
        let signature = generate_signature(secret, body);

        let result = TailscaleWebhook::verify_signature(secret, body, &signature);
        assert!(result.is_ok(), "Empty body with valid signature should verify successfully");
    }
}