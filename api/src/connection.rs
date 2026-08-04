use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ConnectionId;

/// How a connection authenticates with the service it links to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionKind {
    /// An authorisation the user granted through an OAuth 2.0 flow, held as a
    /// refresh token and renewed as needed.
    #[serde(rename = "oauth2")]
    OAuth2,

    /// A token the user obtained from the service and pasted in themselves.
    #[serde(rename = "api_key")]
    ApiKey,

    /// A GitHub App installed on an account, authenticated per-installation.
    #[serde(rename = "github_app")]
    GitHubApp,
}

impl ConnectionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OAuth2 => "oauth2",
            Self::ApiKey => "api_key",
            Self::GitHubApp => "github_app",
        }
    }
}

/// Whether a connection can currently be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    /// Working as far as we know.
    #[default]
    Ok,

    /// The service rejected our credential and the user has to grant access
    /// again. Distinguished from a plain error because it is not going to fix
    /// itself with a retry.
    NeedsReauthorization,

    /// Something else went wrong when the connection was last used.
    Error,
}

impl ConnectionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NeedsReauthorization => "needs_reauthorization",
            Self::Error => "error",
        }
    }

    /// Whether a workflow may currently use this connection.
    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// A connection as described to the browser.
///
/// Deliberately carries no credential — not the token, not a prefix of it, not
/// its length. Everything here is safe to render, log, and include in an
/// exported configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionSummary {
    pub id: ConnectionId,

    /// The service this connects to, e.g. `todoist` or `spotify`.
    pub provider: String,

    pub kind: ConnectionKind,

    /// What to call this connection, defaulting to the account name the
    /// provider reported.
    pub name: String,

    /// The account at the provider, where one could be determined. Shown so
    /// somebody with two accounts on the same service can tell them apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,

    #[serde(default)]
    pub status: ConnectionStatus,

    /// When the current credential expires, for the kinds that expire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_never_carries_a_credential() {
        // The guard is the type: there is nowhere on a summary to put one. This
        // test exists so that adding such a field is a deliberate act that
        // breaks a test named after the reason not to.
        let summary = ConnectionSummary {
            id: ConnectionId::from_entropy(1),
            provider: "todoist".into(),
            kind: ConnectionKind::ApiKey,
            name: "Personal".into(),
            account: Some("alice@example.com".into()),
            status: ConnectionStatus::Ok,
            // Populated so that every optional field is present in the output
            // and the assertion below covers the whole shape.
            expires_at: Some(Utc::now()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let serde_json::Value::Object(rendered) = serde_json::to_value(&summary).unwrap() else {
            panic!("a connection summary should serialise to an object");
        };

        let mut fields: Vec<&str> = rendered.keys().map(String::as_str).collect();
        fields.sort();

        assert_eq!(
            fields,
            vec![
                "account",
                "created_at",
                "expires_at",
                "id",
                "kind",
                "name",
                "provider",
                "status",
                "updated_at",
            ],
            "a field was added to the connection summary; check it cannot carry a credential"
        );
    }

    #[test]
    fn only_a_working_connection_is_usable() {
        assert!(ConnectionStatus::Ok.is_usable());
        assert!(!ConnectionStatus::NeedsReauthorization.is_usable());
        assert!(!ConnectionStatus::Error.is_usable());
    }

    #[test]
    fn statuses_and_kinds_round_trip_through_their_wire_form() {
        for status in [
            ConnectionStatus::Ok,
            ConnectionStatus::NeedsReauthorization,
            ConnectionStatus::Error,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
            assert_eq!(
                serde_json::from_str::<ConnectionStatus>(&json).unwrap(),
                status
            );
        }

        for kind in [
            ConnectionKind::OAuth2,
            ConnectionKind::ApiKey,
            ConnectionKind::GitHubApp,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(serde_json::from_str::<ConnectionKind>(&json).unwrap(), kind);
        }
    }

    #[test]
    fn a_connection_defaults_to_working_when_no_status_was_recorded() {
        let summary: ConnectionSummary = serde_json::from_value(serde_json::json!({
            "id": "abandon-ability",
            "provider": "todoist",
            "kind": "api_key",
            "name": "Personal",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap();

        assert_eq!(summary.status, ConnectionStatus::Ok);
        assert_eq!(summary.expires_at, None);
    }
}
