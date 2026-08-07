//! The registry of people who have signed in.
//!
//! Records live in the [`TenantId::SYSTEM`] namespace rather than in any user's
//! own, because the registry is what tells one user from another — storing it
//! per user would make it unreadable without already knowing who was asking.
//!
//! # What is authoritative
//!
//! A record here is a *description* of an account, not a grant of access.
//! Whether somebody may sign in, and whether they may administer the
//! installation, is decided on every request by evaluating the configured
//! filters against their token claims. That means changing the configuration
//! takes effect immediately, and it means a stale record can never widen
//! somebody's access.
//!
//! The record carries the outcome of that evaluation anyway, so an administrator
//! can see who has which access without replaying everybody's tokens. It is a
//! cache of the last decision, and is treated as such.

use chrono::{DateTime, Utc};

use crate::db::KeyValueStore;
use crate::prelude::*;

/// The key-value partition holding user records, within the system tenant.
pub const USERS_PARTITION: &str = "users";

/// How stale `last_seen_at` may become before a sign-in rewrites the record.
///
/// Every authenticated request passes through here, and the browser polls, so
/// refreshing the timestamp each time would turn a read-mostly registry into a
/// write on every API call. The field only exists to tell an administrator
/// roughly when somebody was last active, so minute-level accuracy is ample.
const LAST_SEEN_RESOLUTION: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

/// A person who has signed in at least once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    /// The account name, which is also the namespace everything they own is
    /// stored under.
    pub username: TenantId,

    /// The name to show for them.
    pub display_name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    /// Whether the administrator filter matched them when they last signed in.
    ///
    /// Informational only: the authoritative check happens per request.
    #[serde(default)]
    pub is_admin: bool,

    /// Whether an administrator has suspended this account.
    ///
    /// Unlike the fields above this *is* authoritative, because it is the only
    /// way to turn somebody off without changing the configured filters.
    #[serde(default)]
    pub disabled: bool,

    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

impl User {
    /// The display identity returned to the browser.
    #[allow(dead_code)]
    pub fn to_admin_user(&self) -> automate_api::AdminUser {
        automate_api::AdminUser {
            name: self.display_name.clone(),
            email: self.email.clone(),
            username: Some(self.username.clone()),
            is_admin: self.is_admin,
            impersonated_by: None,
        }
    }

    /// The account as an administrator sees it in the accounts list.
    pub fn to_account(&self) -> automate_api::Account {
        automate_api::Account {
            username: self.username.clone(),
            display_name: self.display_name.clone(),
            email: self.email.clone(),
            is_admin: self.is_admin,
            disabled: self.disabled,
            first_seen_at: Some(self.first_seen_at),
            last_seen_at: Some(self.last_seen_at),
            reserved: false,
        }
    }
}

/// Reads and writes the user registry.
///
/// Constructed from services already scoped to the system tenant, so the
/// registry cannot be reached from a handler holding an ordinary user's
/// services.
pub struct UserRegistry<S: Services> {
    services: S,
}

impl<S: Services> UserRegistry<S> {
    /// Wraps services that must already be scoped to [`TenantId::SYSTEM`].
    pub fn new(system_services: S) -> Self {
        Self {
            services: system_services,
        }
    }

    /// Looks up a single account.
    pub async fn get(&self, username: &TenantId) -> Result<Option<User>, human_errors::Error> {
        self.services
            .kv()
            .get(USERS_PARTITION, username.to_string())
            .await
    }

    /// Every account that has signed in, ordered by name.
    pub async fn list(&self) -> Result<Vec<User>, human_errors::Error> {
        let mut users: Vec<User> = self
            .services
            .kv()
            .list::<User>(USERS_PARTITION)
            .await?
            .into_iter()
            .map(|(_, user)| user)
            .collect();

        users.sort_by(|a, b| a.username.cmp(&b.username));

        Ok(users)
    }

    /// Records that somebody has just signed in, creating their account if this
    /// is the first time.
    ///
    /// Returns the stored record, or `None` if the account is suspended — in
    /// which case nothing is written, so that suspending somebody also stops
    /// their sign-ins from refreshing the record.
    pub async fn record_sign_in(
        &self,
        username: &TenantId,
        display_name: &str,
        email: Option<&str>,
        is_admin: bool,
    ) -> Result<Option<User>, human_errors::Error> {
        let now = Utc::now();
        let existing = self.get(username).await?;

        if existing.as_ref().is_some_and(|user| user.disabled) {
            return Ok(None);
        }

        let user = User {
            username: username.clone(),
            display_name: display_name.to_string(),
            email: email.map(str::to_string),
            is_admin,
            disabled: false,
            first_seen_at: existing.as_ref().map(|u| u.first_seen_at).unwrap_or(now),
            last_seen_at: now,
        };

        // Skip the write when nothing an administrator would notice has changed,
        // so that a polling browser does not turn every request into a database
        // write.
        if let Some(existing) = &existing
            && existing.display_name == user.display_name
            && existing.email == user.email
            && existing.is_admin == user.is_admin
            && now - existing.last_seen_at < LAST_SEEN_RESOLUTION
        {
            return Ok(Some(existing.clone()));
        }

        self.services
            .kv()
            .set(USERS_PARTITION, username.to_string(), user.clone())
            .await?;

        Ok(Some(user))
    }

    /// Suspends or restores an account.
    pub async fn set_disabled(
        &self,
        username: &TenantId,
        disabled: bool,
    ) -> Result<Option<User>, human_errors::Error> {
        let Some(mut user) = self.get(username).await? else {
            return Ok(None);
        };

        user.disabled = disabled;

        self.services
            .kv()
            .set(USERS_PARTITION, username.to_string(), user.clone())
            .await?;

        Ok(Some(user))
    }

    /// Removes an account from the registry.
    ///
    /// Exposed ahead of the endpoint that will call it, because forgetting and
    /// renaming are the two halves of the recovery path for an account renamed
    /// at the identity provider, and splitting them across releases would leave
    /// that path half-built.
    #[allow(dead_code)]
    ///
    /// Deliberately leaves the records the account owns in place: forgetting who
    /// somebody was should not destroy their workflows, and the two are separate
    /// decisions.
    pub async fn forget(&self, username: &TenantId) -> Result<(), human_errors::Error> {
        self.services
            .kv()
            .remove(USERS_PARTITION, username.to_string())
            .await
    }

    /// Moves a registry entry from one account name to another.
    #[allow(dead_code)]
    ///
    /// Only the registry entry; moving the records the account owns is a
    /// separate step, because it spans every table and belongs to the database
    /// rather than to this registry.
    pub async fn rename(
        &self,
        from: &TenantId,
        to: &TenantId,
    ) -> Result<Option<User>, human_errors::Error> {
        let Some(mut user) = self.get(from).await? else {
            return Ok(None);
        };

        user.username = to.clone();

        self.services
            .kv()
            .set(USERS_PARTITION, to.to_string(), user.clone())
            .await?;
        self.forget(from).await?;

        Ok(Some(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ServicesContainer;

    async fn registry() -> UserRegistry<ServicesContainer<crate::db::TenantDb>> {
        UserRegistry::new(ServicesContainer::new_mock().await.unwrap())
    }

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    #[tokio::test]
    async fn a_first_sign_in_creates_the_account() {
        let registry = registry().await;

        let user = registry
            .record_sign_in(&alice(), "Alice", Some("alice@example.com"), false)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(user.username, alice());
        assert_eq!(user.display_name, "Alice");
        assert_eq!(user.email.as_deref(), Some("alice@example.com"));
        assert!(!user.is_admin);
        assert!(!user.disabled);
        assert_eq!(user.first_seen_at, user.last_seen_at);

        assert_eq!(registry.get(&alice()).await.unwrap(), Some(user));
    }

    #[tokio::test]
    async fn a_later_sign_in_refreshes_the_details_but_keeps_the_first_seen_date() {
        let registry = registry().await;

        let first = registry
            .record_sign_in(&alice(), "Alice", None, false)
            .await
            .unwrap()
            .unwrap();

        // A rename or a promotion at the identity provider should be picked up.
        let second = registry
            .record_sign_in(&alice(), "Alice Smith", Some("alice@example.com"), true)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(second.display_name, "Alice Smith");
        assert_eq!(second.email.as_deref(), Some("alice@example.com"));
        assert!(second.is_admin);
        assert_eq!(
            second.first_seen_at, first.first_seen_at,
            "the account's age should not reset on every sign-in"
        );
        assert!(second.last_seen_at >= first.last_seen_at);
    }

    #[tokio::test]
    async fn repeated_sign_ins_do_not_rewrite_an_unchanged_record() {
        // Every authenticated request records a sign-in and the browser polls,
        // so rewriting the record each time would turn a read-mostly registry
        // into a write on every API call.
        let registry = registry().await;

        let first = registry
            .record_sign_in(&alice(), "Alice", None, false)
            .await
            .unwrap()
            .unwrap();
        let second = registry
            .record_sign_in(&alice(), "Alice", None, false)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            first.last_seen_at, second.last_seen_at,
            "an unchanged record within the resolution window should not be rewritten"
        );
    }

    #[tokio::test]
    async fn a_change_to_the_details_is_written_through_immediately() {
        // The write is skipped only when nothing an administrator would notice
        // has changed, so a promotion must not wait for the timestamp window.
        let registry = registry().await;

        registry
            .record_sign_in(&alice(), "Alice", None, false)
            .await
            .unwrap();
        let promoted = registry
            .record_sign_in(&alice(), "Alice", None, true)
            .await
            .unwrap()
            .unwrap();

        assert!(promoted.is_admin);
        assert!(registry.get(&alice()).await.unwrap().unwrap().is_admin);
    }

    #[tokio::test]
    async fn a_suspended_account_cannot_sign_in_or_refresh_its_record() {
        let registry = registry().await;

        registry
            .record_sign_in(&alice(), "Alice", None, false)
            .await
            .unwrap();
        registry.set_disabled(&alice(), true).await.unwrap();

        // Suspension is the one flag the registry decides rather than the
        // configured filters, so it must survive a sign-in attempt.
        assert_eq!(
            registry
                .record_sign_in(&alice(), "Alice", None, true)
                .await
                .unwrap(),
            None
        );

        let stored = registry.get(&alice()).await.unwrap().unwrap();
        assert!(stored.disabled);
        assert!(
            !stored.is_admin,
            "a suspended account must not be able to record itself as an administrator"
        );
    }

    #[tokio::test]
    async fn restoring_an_account_lets_it_sign_in_again() {
        let registry = registry().await;

        registry
            .record_sign_in(&alice(), "Alice", None, false)
            .await
            .unwrap();
        registry.set_disabled(&alice(), true).await.unwrap();
        registry.set_disabled(&alice(), false).await.unwrap();

        assert!(
            registry
                .record_sign_in(&alice(), "Alice", None, false)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn accounts_are_listed_in_a_stable_order() {
        let registry = registry().await;

        for name in ["carol", "alice", "bob"] {
            registry
                .record_sign_in(&TenantId::new(name).unwrap(), name, None, false)
                .await
                .unwrap();
        }

        let names: Vec<String> = registry
            .list()
            .await
            .unwrap()
            .iter()
            .map(|u| u.username.to_string())
            .collect();

        assert_eq!(names, vec!["alice", "bob", "carol"]);
    }

    #[tokio::test]
    async fn renaming_moves_the_entry_and_leaves_nothing_behind() {
        let registry = registry().await;
        let renamed = TenantId::new("alice.smith").unwrap();

        registry
            .record_sign_in(&alice(), "Alice", None, true)
            .await
            .unwrap();
        registry.rename(&alice(), &renamed).await.unwrap();

        assert_eq!(registry.get(&alice()).await.unwrap(), None);

        let moved = registry.get(&renamed).await.unwrap().unwrap();
        assert_eq!(moved.username, renamed);
        assert!(
            moved.is_admin,
            "renaming must not change what somebody may do"
        );
    }

    #[tokio::test]
    async fn forgetting_an_unknown_account_is_not_an_error() {
        let registry = registry().await;

        assert!(registry.forget(&alice()).await.is_ok());
        assert_eq!(registry.set_disabled(&alice(), true).await.unwrap(), None);
        assert_eq!(registry.rename(&alice(), &alice()).await.unwrap(), None);
    }
}
