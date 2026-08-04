use serde::{Deserialize, Serialize};

use crate::TenantId;

/// The identity of the signed-in user, derived from the validated OIDC token
/// claims.
///
/// The optional fields are absent in an installation with no identity provider
/// configured, where there is nobody to identify and access is governed by
/// request metadata alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminUser {
    /// The name to show for this person.
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    /// The account everything this person owns is stored under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<TenantId>,

    /// Whether this session may administer the installation, which includes
    /// acting as another user.
    #[serde(default)]
    pub is_admin: bool,

    /// The administrator acting as this user, when one is.
    ///
    /// Present only during impersonation, and always names the administrator
    /// rather than the person being impersonated — so the UI can say whose
    /// records are being changed and on whose authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impersonated_by: Option<TenantId>,
}

impl AdminUser {
    /// Builds the display identity for a user with no elevated access.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: None,
            username: None,
            is_admin: false,
            impersonated_by: None,
        }
    }

    /// Whether an administrator is currently acting as this user.
    pub fn is_impersonated(&self) -> bool {
        self.impersonated_by.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unauthenticated_installation_serialises_only_a_name() {
        // With no identity provider there is nobody to identify, and the extra
        // fields would be misleading rather than merely empty.
        let user = AdminUser::new("Signed in");
        let json = serde_json::to_value(&user).unwrap();

        assert_eq!(
            json,
            serde_json::json!({ "name": "Signed in", "is_admin": false })
        );
    }

    #[test]
    fn a_record_written_before_these_fields_existed_still_loads() {
        let user: AdminUser = serde_json::from_str(r#"{"name":"Alice"}"#).unwrap();

        assert_eq!(user.name, "Alice");
        assert_eq!(user.username, None);
        assert!(!user.is_admin);
        assert!(!user.is_impersonated());
    }

    #[test]
    fn impersonation_names_the_administrator_rather_than_the_subject() {
        let user = AdminUser {
            impersonated_by: Some(TenantId::new("admin").unwrap()),
            ..AdminUser::new("Alice")
        };

        assert!(user.is_impersonated());
        assert_eq!(
            user.impersonated_by.as_ref().map(|t| t.as_str()),
            Some("admin")
        );
    }
}
