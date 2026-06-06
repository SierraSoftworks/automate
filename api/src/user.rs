use serde::{Deserialize, Serialize};

/// The identity of the currently signed-in administrator, derived from the
/// validated OIDC token claims.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminUser {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}
