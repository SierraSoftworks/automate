use serde::{Deserialize, Serialize};

/// An external service which is configured on this instance and can be
/// connected through the setup wizard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationInfo {
    /// The URL-safe identifier, e.g. `github` or `spotify`.
    pub id: String,
    /// The name shown to a human.
    pub name: String,
}

/// An account which has been connected to an integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// Identifies the connection within its integration, and is what
    /// `DELETE /api/v1/integrations/{integration}/connections/{id}` addresses.
    /// Its form is the integration's business: a GitHub App installation id, an
    /// OAuth2 provider's queue message key, and so on.
    pub id: String,
    /// The name shown to a human.
    pub name: String,
    /// An optional qualifier such as `User` or `Organization`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// An optional secondary line, such as when the connection's credentials
    /// next need renewing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Connection {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind: None,
            detail: None,
        }
    }

    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}
