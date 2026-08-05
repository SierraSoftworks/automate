//! In-flight authorisations.
//!
//! When somebody starts linking a service, the agent sends them to the provider
//! and has to recognise them when they come back. The `state` parameter carries
//! that thread, and the browser also holds it in a short-lived cookie so a
//! callback that was not started here can be rejected.
//!
//! # Why the account is remembered here rather than read from the request
//!
//! The callback has to land the resulting credential in the account that started
//! the authorisation. Taking that from the request — from the session, or from a
//! cookie — would mean an attacker who can influence either could have somebody
//! else's freshly granted credential filed under an account they control, or
//! have their own filed under a victim's so that the victim's workflows quietly
//! start acting on the attacker's data.
//!
//! So the account is recorded here, server-side, at the moment the authorisation
//! begins, and the callback uses what it finds rather than what it is told. The
//! cookie remains as a check that the callback belongs to this browser, but it
//! no longer decides anything.
//!
//! Records live in the system namespace, because a pending authorisation is not
//! yet anybody's: it is precisely the thing that decides whose it will be.

use chrono::{DateTime, Utc};

use crate::db::KeyValueStore;
use crate::prelude::*;

/// The key-value partition holding pending authorisations, in the system tenant.
pub const PENDING_PARTITION: &str = "oauth-state";

/// How long somebody has to complete an authorisation before it is abandoned.
///
/// Long enough to sign in to a provider, approve a consent screen and deal with
/// a second factor, and short enough that an abandoned attempt is not left
/// waiting to be picked up.
const LIFETIME: chrono::TimeDelta = chrono::TimeDelta::minutes(10);

/// An authorisation which has begun but not yet come back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingAuthorization {
    /// The account that will own the resulting credential.
    pub tenant: TenantId,

    /// Which integration was being set up, so a state issued for one cannot be
    /// redeemed against another.
    pub integration: String,

    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl PendingAuthorization {
    fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// Records authorisations between starting them and their callback arriving.
pub struct PendingAuthorizations<S: Services> {
    services: S,
}

impl<S: Services> PendingAuthorizations<S> {
    /// Wraps services that must already be scoped to [`TenantId::SYSTEM`].
    pub fn new(system_services: S) -> Self {
        Self {
            services: system_services,
        }
    }

    /// Remembers which account an authorisation belongs to.
    pub async fn begin(
        &self,
        state: &str,
        tenant: TenantId,
        integration: &str,
    ) -> Result<(), human_errors::Error> {
        let now = Utc::now();

        self.services
            .kv()
            .set(
                PENDING_PARTITION,
                state.to_string(),
                PendingAuthorization {
                    tenant,
                    integration: integration.to_string(),
                    started_at: now,
                    expires_at: now + LIFETIME,
                },
            )
            .await
    }

    /// Redeems an authorisation, returning the account that started it.
    ///
    /// One-shot: the record is removed whether or not it turns out to be usable,
    /// so a callback URL cannot be replayed to mint a second credential.
    pub async fn claim(
        &self,
        state: &str,
        integration: &str,
    ) -> Result<PendingAuthorization, human_errors::Error> {
        let pending: Option<PendingAuthorization> = self
            .services
            .kv()
            .get(PENDING_PARTITION, state.to_string())
            .await?;

        self.services
            .kv()
            .remove(PENDING_PARTITION, state.to_string())
            .await?;

        let Some(pending) = pending else {
            return Err(unrecognised());
        };

        if pending.is_expired(Utc::now()) {
            return Err(human_errors::user(
                "This setup link has expired.",
                &["Start connecting the service again."],
            ));
        }

        // A state issued while connecting one service must not be redeemable
        // against another, which would otherwise let a credential be filed under
        // the wrong provider.
        if pending.integration != integration {
            warn!(
                expected = %pending.integration,
                actual = %integration,
                "Refused a setup callback whose state was issued for a different integration."
            );
            return Err(unrecognised());
        }

        Ok(pending)
    }

    /// Discards authorisations that were never completed.
    ///
    /// Called by housekeeping rather than on the request path, so that an
    /// abandoned attempt costs a row until the next sweep instead of adding a
    /// scan to every sign-in.
    #[allow(dead_code)]
    pub async fn prune(&self) -> Result<usize, human_errors::Error> {
        let now = Utc::now();
        let pending = self
            .services
            .kv()
            .list::<PendingAuthorization>(PENDING_PARTITION)
            .await?;

        let mut removed = 0;
        for (state, authorization) in pending {
            if authorization.is_expired(now) {
                self.services.kv().remove(PENDING_PARTITION, state).await?;
                removed += 1;
            }
        }

        Ok(removed)
    }
}

/// The response to a state we do not recognise.
///
/// Deliberately the same whether the state was never issued, has already been
/// used, or was issued for something else, so that probing cannot tell them
/// apart.
fn unrecognised() -> human_errors::Error {
    human_errors::user(
        "We could not match this callback to a setup you started.",
        &[
            "Start connecting the service again from this application.",
            "If you opened the link in a different browser or profile, try again in the one you started from.",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{AppContext, ServicesContainer};

    async fn store() -> (
        AppContext,
        PendingAuthorizations<ServicesContainer<crate::db::TenantDb>>,
    ) {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let store = PendingAuthorizations::new(context.tenant(TenantId::system()));

        (context, store)
    }

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    #[tokio::test]
    async fn a_callback_is_attributed_to_the_account_that_started_it() {
        let (_context, store) = store().await;

        store.begin("state-1", alice(), "spotify").await.unwrap();

        let claimed = store.claim("state-1", "spotify").await.unwrap();
        assert_eq!(claimed.tenant, alice());
        assert_eq!(claimed.integration, "spotify");
    }

    #[tokio::test]
    async fn a_state_can_only_be_redeemed_once() {
        // Otherwise a leaked callback URL could be replayed to mint a second
        // credential after the first had been revoked.
        let (_context, store) = store().await;

        store.begin("state-1", alice(), "spotify").await.unwrap();
        store.claim("state-1", "spotify").await.unwrap();

        assert!(store.claim("state-1", "spotify").await.is_err());
    }

    #[tokio::test]
    async fn a_state_issued_for_one_service_cannot_be_redeemed_against_another() {
        let (_context, store) = store().await;

        store.begin("state-1", alice(), "spotify").await.unwrap();

        assert!(store.claim("state-1", "todoist").await.is_err());
        assert!(
            store.claim("state-1", "spotify").await.is_err(),
            "a rejected claim should still consume the state"
        );
    }

    #[tokio::test]
    async fn an_unrecognised_state_is_refused() {
        let (_context, store) = store().await;

        assert!(store.claim("never-issued", "spotify").await.is_err());
    }

    #[tokio::test]
    async fn an_abandoned_authorisation_expires() {
        let (context, store) = store().await;
        let system = context.tenant(TenantId::system());

        let started = Utc::now() - chrono::Duration::hours(1);
        system
            .kv()
            .set(
                PENDING_PARTITION,
                "state-1".to_string(),
                PendingAuthorization {
                    tenant: alice(),
                    integration: "spotify".into(),
                    started_at: started,
                    expires_at: started + LIFETIME,
                },
            )
            .await
            .unwrap();

        let err = store.claim("state-1", "spotify").await.unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
    }

    #[tokio::test]
    async fn expired_authorisations_can_be_cleared_out() {
        let (context, store) = store().await;
        let system = context.tenant(TenantId::system());

        let stale = Utc::now() - chrono::Duration::hours(1);
        system
            .kv()
            .set(
                PENDING_PARTITION,
                "stale".to_string(),
                PendingAuthorization {
                    tenant: alice(),
                    integration: "spotify".into(),
                    started_at: stale,
                    expires_at: stale + LIFETIME,
                },
            )
            .await
            .unwrap();
        store.begin("fresh", alice(), "spotify").await.unwrap();

        assert_eq!(store.prune().await.unwrap(), 1);
        assert!(store.claim("fresh", "spotify").await.is_ok());
    }

    #[tokio::test]
    async fn the_refusal_does_not_reveal_why_a_state_was_rejected() {
        // Probing must not be able to tell "never issued" from "already used"
        // or "issued for something else".
        let (_context, store) = store().await;

        store.begin("used", alice(), "spotify").await.unwrap();
        store.claim("used", "spotify").await.unwrap();

        store
            .begin("wrong-service", alice(), "spotify")
            .await
            .unwrap();

        let never = store.claim("never-issued", "spotify").await.unwrap_err();
        let used = store.claim("used", "spotify").await.unwrap_err();
        let mismatched = store.claim("wrong-service", "todoist").await.unwrap_err();

        assert_eq!(never.to_string(), used.to_string());
        assert_eq!(never.to_string(), mismatched.to_string());
    }
}
