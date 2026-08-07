use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::TenantId;

/// An account an administrator can see, suspend, or act as.
///
/// Almost every account is a person who has signed in, and is held in the user
/// registry. The installation's own account is not — nobody signs into it — but
/// it owns records all the same, and after `multi_tenant` is switched on it owns
/// every record that predates the change. It is listed here so that it can be
/// reached, marked [`reserved`](Self::reserved) and without the sign-in dates it
/// has no honest answer for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// The account name, which is also the namespace everything it owns is
    /// stored under.
    pub username: TenantId,

    /// The name to show for it.
    pub display_name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    /// Whether the administrator filter matched when they last signed in.
    #[serde(default)]
    pub is_admin: bool,

    /// Whether an administrator has suspended this account.
    #[serde(default)]
    pub disabled: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,

    /// Owned by the installation rather than by a person, so it can be acted as
    /// but never suspended — there is nobody to lock out, and suspending it
    /// would only make its records unreachable.
    #[serde(default)]
    pub reserved: bool,
}

impl Account {
    /// The installation's own account, which nobody has signed into.
    pub fn reserved(username: TenantId, display_name: impl Into<String>) -> Self {
        Self {
            username,
            display_name: display_name.into(),
            email: None,
            is_admin: false,
            disabled: false,
            first_seen_at: None,
            last_seen_at: None,
            reserved: true,
        }
    }
}

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
