use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::{prelude::*, publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate}};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Deserialize, Default)]
pub struct TailscaleWebhookConfig {
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
pub struct TailscaleWebhook;

impl TailscaleWebhook {
    fn parse_signature(header: &str) -> Result<(chrono::DateTime<Utc>, Vec<u8>), human_errors::Error> {
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
                let timestamp = timestamp.parse().ok()
                    .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                    .ok_or_else(|| human_errors::user(
                        "The timestamp in the Tailscale-Webhook-Signature header is invalid.",
                        &[
                            "Ensure that you are only sending Tailscale webhooks to this endpoint.",
                            "Check that the webhook is configured correctly at https://login.tailscale.com/admin/settings/webhooks"
                        ]
                    ))?;

                let signature = hex::decode(signature).map_err_as_user(&[
                    "The signature in the Tailscale-Webhook-Signature header is not valid hex.",
                    "Ensure that you are only sending Tailscale webhooks to this endpoint.",
                    "Check that the webhook is configured correctly at https://login.tailscale.com/admin/settings/webhooks"
                ])?;

                Ok((timestamp, signature))

            },
            _ => Err(
                human_errors::user(
                    "The X-Tailscale-Webhook-Signature header did not contain a valid signature.",
                    &[
                        "Ensure that you are only sending Tailscale webhooks to this endpoint.",
                        "Check that the webhook is configured correctly at https://login.tailscale.com/admin/settings/webhooks"
                    ]
                )
            )
        }
    }

    /// Verifies the Tailscale webhook signature.
    /// 
    /// According to https://tailscale.com/kb/1213/webhooks#verifying-an-event-signature,
    /// Tailscale signs webhooks using HMAC-SHA256 with the webhook secret, and includes
    /// the signature in the Tailscale-Webhook-Signature header in the format:
    /// `t=<timestamp>,v1=<hex_signature>`
    fn verify_signature(secret: &str, body: &str, signature_header: &str) -> Result<(), human_errors::Error> {
        let (timestamp, expected_signature) = Self::parse_signature(signature_header)?;

        if (timestamp - Utc::now()).abs() > chrono::Duration::minutes(5) {
            return Err(human_errors::user(
                "The Tailscale webhook signature timestamp is too old or too far in the future.",
                &[
                    "Ensure that the system clock on this server is accurate.",
                    "Check that the webhook is configured correctly at https://login.tailscale.com/admin/settings/webhooks"
                ]
            ));
        }
        
        // Create the string to sign: <timestamp>.<body>
        let string_to_sign = format!("{}.{}", timestamp.timestamp(), body);
        
        // Create HMAC-SHA256 instance with the secret
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .wrap_err_as_user(
                "Failed to create HMAC instance with the provided secret.",
                &[
                    "Ensure that you have provided a valid webhooks.tailscale.secret in your configuration.",
                    "Ensure that the configured webhooks.tailscale.secret matches that on https://login.tailscale.com/admin/settings/webhooks"
                ]
            )?;
        
        // Compute the HMAC of the string to sign
        mac.update(string_to_sign.as_bytes());

        // Verify the signature
        mac.verify_slice(&expected_signature)
            .wrap_err_as_user(
                format!("Webhook signature verification failed (signatures did not match)."),
                &["Ensure that the configured webhooks.tailscale.secret matches that on https://login.tailscale.com/admin/settings/webhooks"]
            )?;
        
        Ok(())
    }
}

impl Job for TailscaleWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/tailscale"
    }

    #[instrument("webhooks.tailscale.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(&self, job: &Self::JobType, services: impl Services + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        // Validate the Tailscale webhook signature header
        // https://tailscale.com/kb/1213/webhooks#verifying-an-event-signature
        let secret = &services.config().webhooks.tailscale.secret;
        
        if !secret.is_empty() {
            // HTTP headers are case-insensitive, so we need to search for the header with case-insensitive comparison
            let signature = job.headers.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("tailscale-webhook-signature"))
                .map(|(_, value)| value.as_str());
            
            if let Some(signature) = signature {
                Self::verify_signature(secret, &job.body, signature)?;
            } else {
                warn!("Received Tailscale webhook without signature, but secret is configured; rejecting request.");
                return Ok(());
            }
        } else {
            debug!("No Tailscale webhook secret configured; skipping signature verification.");
        }

        let event: TailscaleAlertEventPayload = job.json()?;

        if !services.config().webhooks.tailscale.filter.matches(&event)? {
            info!("Tailscale event '{}' did not match filter; ignoring.", event._type);
            return Ok(());
        }

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

impl Filterable for TailscaleAlertEventPayload {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "type" => self._type.clone().into(),
            "tailnet" => self.tailnet.clone().into(),
            "message" => self.message.clone().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Helper function to generate a valid signature for testing
    /// Returns the signature in Tailscale format: t=<timestamp>,v1=<hex_signature>
    fn generate_signature(secret: &str, timestamp: &str, body: &str) -> String {
        let string_to_sign = format!("{}.{}", timestamp, body);
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(string_to_sign.as_bytes());
        let result = mac.finalize();
        let hex_sig = hex::encode(result.into_bytes());
        format!("t={},v1={}", timestamp, hex_sig)
    }

    #[test]
    fn test_verify_signature_valid() {
        let secret = "test_secret_key";
        let timestamp = Utc::now().timestamp().to_string();
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let signature = generate_signature(secret, &timestamp, body);

        let result = TailscaleWebhook::verify_signature(secret, body, &signature);
        result.expect("Valid signature should verify successfully");
    }

    #[test]
    fn test_verify_signature_invalid() {
        let secret = "test_secret_key";
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let wrong_signature = "t=1663781880,v1=0000000000000000000000000000000000000000000000000000000000000000";

        let result = TailscaleWebhook::verify_signature(secret, body, wrong_signature);
        assert!(result.is_err(), "Invalid signature should fail verification");
    }

    #[test]
    fn test_verify_signature_wrong_secret() {
        let secret = "test_secret_key";
        let wrong_secret = "wrong_secret_key";
        let timestamp = Utc::now().timestamp().to_string();
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let signature = generate_signature(wrong_secret, &timestamp, body);

        let result = TailscaleWebhook::verify_signature(secret, body, &signature);
        assert!(result.is_err(), "Signature with wrong secret should fail verification");
    }

    #[test]
    fn test_verify_signature_tampered_body() {
        let secret = "test_secret_key";
        let timestamp = Utc::now().timestamp().to_string();
        let original_body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let tampered_body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Tampered message","data":{}}"#;
        let signature = generate_signature(secret, &timestamp, original_body);

        let result = TailscaleWebhook::verify_signature(secret, tampered_body, &signature);
        assert!(result.is_err(), "Tampered body should fail verification");
    }

    #[test]
    fn test_verify_signature_invalid_format() {
        let secret = "test_secret_key";
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let invalid_format = "not_a_valid_format";

        let result = TailscaleWebhook::verify_signature(secret, body, invalid_format);
        assert!(result.is_err(), "Invalid format should fail");
    }

    #[test]
    fn test_verify_signature_missing_timestamp() {
        let secret = "test_secret_key";
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let missing_timestamp = "v1=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        let result = TailscaleWebhook::verify_signature(secret, body, missing_timestamp);
        assert!(result.is_err(), "Missing timestamp should fail");
    }

    #[test]
    fn test_verify_signature_missing_signature() {
        let secret = "test_secret_key";
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test message","data":{}}"#;
        let missing_signature = "t=1663781880";

        let result = TailscaleWebhook::verify_signature(secret, body, missing_signature);
        assert!(result.is_err(), "Missing signature should fail");
    }

    #[test]
    fn test_verify_signature_empty_body() {
        let secret = "test_secret_key";
        let timestamp = Utc::now().timestamp().to_string();
        let body = "";
        let signature = generate_signature(secret, &timestamp, body);

        let result = TailscaleWebhook::verify_signature(secret, body, &signature);
        result.expect("Empty body with valid signature should verify successfully");
    }

    #[test]
    fn test_header_lookup_case_insensitive() {
        // Test that header lookup works with different case variations
        let body = r#"{"version":1,"timestamp":"2024-01-01T00:00:00Z","type":"test","tailnet":"example.com","message":"Test","data":{}}"#;
        let signature = generate_signature("secret", "1663781880", body);
        
        // Test with lowercase
        let mut headers = HashMap::new();
        headers.insert("tailscale-webhook-signature".to_string(), signature.clone());
        
        let found = headers.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("tailscale-webhook-signature"))
            .map(|(_, value)| value.as_str());
        assert!(found.is_some(), "Should find lowercase header");
        
        // Test with uppercase
        let mut headers = HashMap::new();
        headers.insert("TAILSCALE-WEBHOOK-SIGNATURE".to_string(), signature.clone());
        
        let found = headers.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("tailscale-webhook-signature"))
            .map(|(_, value)| value.as_str());
        assert!(found.is_some(), "Should find uppercase header");
        
        // Test with mixed case
        let mut headers = HashMap::new();
        headers.insert("Tailscale-Webhook-Signature".to_string(), signature.clone());
        
        let found = headers.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("tailscale-webhook-signature"))
            .map(|(_, value)| value.as_str());
        assert!(found.is_some(), "Should find mixed case header");
    }
}