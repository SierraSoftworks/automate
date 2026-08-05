//! Who a request is acting as.
//!
//! Two questions have to be answered before a request touches any stored record,
//! and they are not the same question:
//!
//! - **Who signed in?** The account whose credential was presented, and whose
//!   name belongs on anything written to the audit log.
//! - **Whose records are we acting on?** Normally the same account — but an
//!   administrator may act as somebody else in order to help them, in which case
//!   the two diverge.
//!
//! Keeping both on the [`Principal`] is what lets impersonation be safe. The
//! effective account decides which tenant is reached, while permission checks
//! and audit entries continue to name the administrator, so acting as somebody
//! else can never be used to escape attribution.

use automate_api::TenantId;

/// The identity a request is being handled under.
#[derive(Debug, Clone)]
pub struct Principal {
    /// The account that authenticated.
    actor: TenantId,

    /// The account whose records this request operates on.
    effective: TenantId,

    /// Whether the *actor* may administer the installation.
    ///
    /// Always a property of the person who signed in, never of the account they
    /// are acting as, so impersonating an administrator confers nothing.
    is_admin: bool,

    /// How the signed-in person should be shown, where there is one to show.
    user: Option<automate_api::AdminUser>,
}

impl Principal {
    /// A request acting as the account that authenticated.
    pub fn new(account: TenantId, is_admin: bool, user: Option<automate_api::AdminUser>) -> Self {
        Self {
            effective: account.clone(),
            actor: account,
            is_admin,
            user,
        }
    }

    /// Redirects this principal to act on another account's records.
    ///
    /// The actor and the administrator flag are left alone, so the request keeps
    /// the permissions and the attribution of whoever signed in.
    pub fn impersonating(mut self, subject: TenantId) -> Self {
        self.effective = subject;
        self
    }

    /// The account that authenticated, and to whom actions are attributed.
    pub fn actor(&self) -> &TenantId {
        &self.actor
    }

    /// The account whose records this request operates on.
    pub fn effective(&self) -> &TenantId {
        &self.effective
    }

    /// Whether the signed-in account may administer the installation.
    pub fn is_admin(&self) -> bool {
        self.is_admin
    }

    /// Whether this request is acting as somebody other than who signed in.
    pub fn is_impersonating(&self) -> bool {
        self.actor != self.effective
    }

    /// The identity to show in the browser.
    ///
    /// While impersonating, this describes the account being acted upon but
    /// names the administrator in `impersonated_by`, so the UI can make it
    /// obvious whose records are on screen and on whose authority.
    pub fn to_admin_user(&self) -> Option<automate_api::AdminUser> {
        let user = self.user.as_ref()?;

        Some(automate_api::AdminUser {
            username: Some(self.effective.clone()),
            is_admin: self.is_admin,
            impersonated_by: self.is_impersonating().then(|| self.actor.clone()),
            ..user.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admin() -> TenantId {
        TenantId::new("admin").unwrap()
    }

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    fn principal(account: TenantId, is_admin: bool) -> Principal {
        Principal::new(
            account.clone(),
            is_admin,
            Some(automate_api::AdminUser::new(account.to_string())),
        )
    }

    #[test]
    fn an_ordinary_request_acts_as_the_account_that_signed_in() {
        let principal = principal(alice(), false);

        assert_eq!(principal.actor(), &alice());
        assert_eq!(principal.effective(), &alice());
        assert!(!principal.is_impersonating());
        assert!(!principal.is_admin());
    }

    #[test]
    fn impersonation_moves_the_records_reached_but_not_the_attribution() {
        let principal = principal(admin(), true).impersonating(alice());

        assert_eq!(
            principal.effective(),
            &alice(),
            "the request should reach the impersonated account's records"
        );
        assert_eq!(
            principal.actor(),
            &admin(),
            "actions must stay attributed to whoever signed in"
        );
        assert!(principal.is_impersonating());
    }

    #[test]
    fn impersonating_someone_does_not_change_what_the_actor_may_do() {
        // Administrator status belongs to whoever signed in. If it followed the
        // impersonated account instead, acting as an administrator would be a
        // way to become one.
        let elevated = principal(admin(), true).impersonating(alice());
        assert!(elevated.is_admin());

        let ordinary = principal(alice(), false).impersonating(admin());
        assert!(
            !ordinary.is_admin(),
            "acting as an administrator must not grant administrator access"
        );
    }

    #[test]
    fn the_displayed_identity_names_the_administrator_behind_an_impersonation() {
        let principal = principal(admin(), true).impersonating(alice());
        let shown = principal.to_admin_user().unwrap();

        assert_eq!(shown.username, Some(alice()));
        assert_eq!(shown.impersonated_by, Some(admin()));
        assert!(shown.is_impersonated());
    }

    #[test]
    fn an_ordinary_session_is_not_reported_as_impersonated() {
        let shown = principal(alice(), false).to_admin_user().unwrap();

        assert_eq!(shown.username, Some(alice()));
        assert_eq!(shown.impersonated_by, None);
        assert!(!shown.is_impersonated());
    }

    #[test]
    fn an_installation_without_an_identity_provider_has_nobody_to_show() {
        // Access is governed by request metadata alone, so presenting a name
        // would invent an identity that does not exist.
        let principal = Principal::new(TenantId::local(), true, None);

        assert_eq!(principal.effective(), &TenantId::local());
        assert!(principal.to_admin_user().is_none());
    }
}
