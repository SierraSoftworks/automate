//! Keeping stored OAuth grants current.
//!
//! A grant used to be renewed only when something reached for it, which works
//! for an account whose workflows run constantly and fails quietly for one
//! whose workflows do not. Providers expire a refresh token that has gone
//! unused — Spotify, YNAB and Todoist all do — so an account that was merely
//! quiet for long enough comes back to a grant that is dead rather than stale,
//! and the only way out of that is for the person to authorise again.
//!
//! This sweeps every account's connections on a fixed interval and renews the
//! ones approaching expiry, which does two things. The refresh token is
//! exercised often enough that no provider drops it for disuse, and the access
//! token a workflow finds stored is one it can use as it stands.
//!
//! # Why this is not a queued job
//!
//! For the same reason the audit log is trimmed from [`crate::job::JobHost`]
//! rather than a handler: it reaches across accounts, and a queue partition any
//! tenant could enqueue into is the wrong shape for work that touches
//! everybody's credentials. Nobody configures this and nobody can delete it.

use chrono::Utc;

use automate_api::ConnectionKind;

use crate::connections::{Connection, ConnectionStore, RENEW_BEFORE};
use crate::db::{AuditCategory, AuditEntry, AuditOutcome, AuditStore};
use crate::integrations::{RefreshOutcome, Registry};
use crate::prelude::*;
use crate::services::{AppContext, AppServices};

/// How often every account's connections are swept.
///
/// Comfortably shorter than [`RENEW_BEFORE`], so a grant that falls due between
/// two sweeps is still renewed with time to spare — which is the guarantee that
/// lets a workflow treat the stored access token as usable.
const EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Renews the grants approaching expiry, and keeps doing so.
///
/// Runs on start-up as well as on the interval, so an agent that has been down
/// long enough for tokens to lapse renews them before the first schedule fires
/// rather than after it has already failed.
pub async fn run(context: AppContext) {
    let registry = match Registry::new(&context.config()) {
        Ok(registry) => registry,
        Err(err) => {
            // The web server resolves the same registry and refuses to start on
            // this, so reaching it here means the process is already on its way
            // down; there is nothing useful left for this task to do.
            error!(error = %err, "Could not resolve the integrations, so stored credentials will not be renewed in the background: {err}");
            context.session().record_human_error(&err);
            return;
        }
    };

    loop {
        if let Err(err) = sweep(&context, &registry).await {
            // Not fatal and not retried sooner. The next sweep is minutes away,
            // and the renewal each workflow still performs on its way to a token
            // stands behind this one.
            error!(error = %err, "Failed to renew stored credentials: {err}");
            context.session().record_human_error(&err);
        }

        tokio::time::sleep(EVERY).await;
    }
}

/// Offers every account's due connections to the integration that owns them.
#[instrument("connections.refresh.sweep", skip(context, registry), err(Display))]
async fn sweep(context: &AppContext, registry: &Registry) -> Result<(), human_errors::Error> {
    let mut reviewed = 0usize;
    let mut expired = 0usize;

    for tenant in context.database().tenants().await? {
        let services = context.tenant(tenant.clone());
        let store = ConnectionStore::for_services(&services);

        // One account's unreadable connections are not a reason to stop renewing
        // everybody else's.
        let connections = match store.list().await {
            Ok(connections) => connections,
            Err(err) => {
                error!(tenant = %tenant, error = %err, "Could not read an account's connections while renewing credentials: {err}");
                services.session().record_human_error(&err);
                continue;
            }
        };

        for connection in connections.iter().filter(|c| is_due(c)) {
            // A provider we no longer have configured leaves its connections
            // alone rather than failing: the credential is still the user's, and
            // restoring the configuration is what brings it back.
            let Some((integration, _)) = registry.get(&connection.provider) else {
                continue;
            };

            match integration.refresh(connection, &services).await {
                Ok(RefreshOutcome::Current) => reviewed += 1,
                Ok(RefreshOutcome::NeedsReauthorization) => {
                    expired += 1;
                    report_expiry(&services, connection).await;
                }
                Err(err) => {
                    // Transient by construction — a provider that has rejected
                    // the grant reports it as an outcome rather than an error —
                    // so the next sweep tries again.
                    error!(
                        tenant = %tenant,
                        connection.id = %connection.id,
                        error = %err,
                        "Could not renew a stored credential: {err}",
                    );
                    services.session().record_human_error(&err);
                }
            }
        }
    }

    debug!("Reviewed {reviewed} expiring credential(s); {expired} need re-authorization.");

    Ok(())
}

/// Whether a connection holds a grant close enough to expiry to renew.
///
/// Decided from the expiry denormalised onto the record, so a sweep does not
/// decrypt every credential every few minutes only to discover it had nothing
/// to do. A connection already marked as broken is left alone: renewing it
/// cannot succeed, and re-asking the provider every sweep would be a request
/// per connection per interval spent proving that.
fn is_due(connection: &Connection) -> bool {
    connection.kind == ConnectionKind::OAuth2
        && connection.status.is_usable()
        && connection
            .expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now() + RENEW_BEFORE)
}

/// Records that an account's grant has died, so the person owning it finds out
/// from the Activity page rather than from a workflow that stopped working.
async fn report_expiry(services: &AppServices, connection: &Connection) {
    let entry = AuditEntry::new(
        AuditCategory::Connection,
        "needs-reauthorization",
        AuditOutcome::Failure,
    )
    .subject(connection.id)
    .message(format!(
        "{} will no longer accept this connection's authorization. Reconnect the account to resume the workflows that use it.",
        connection.provider
    ));

    if let Err(err) = services.audit().record(entry).await {
        warn!(error = %err, "Could not record that a connection needs re-authorization: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::ConnectionSecret;
    use crate::services::AppContext;
    use automate_api::ConnectionStatus;
    use chrono::Duration;

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    async fn connection(secret: ConnectionSecret) -> Connection {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let store = ConnectionStore::new(context.tenant(alice()), alice());

        store
            .create("spotify", "Alice", Some("alice".into()), secret)
            .await
            .unwrap()
    }

    fn oauth2(expires_in: Duration) -> ConnectionSecret {
        ConnectionSecret::OAuth2 {
            access_token: "access-tYqR9".into(),
            refresh_token: "refresh-Kx3Lm".into(),
            expires_at: Utc::now() + expires_in,
        }
    }

    /// The point of the sweep: a grant is renewed before it expires, not once a
    /// workflow has already found it expired.
    #[tokio::test]
    async fn a_grant_inside_the_renewal_window_is_due() {
        assert!(is_due(
            &connection(oauth2(RENEW_BEFORE - Duration::minutes(1))).await
        ));
    }

    /// Renewing on every sweep regardless would spend a token exchange per
    /// connection per interval, and rotate refresh tokens far more often than
    /// any provider expects.
    #[tokio::test]
    async fn a_grant_with_life_left_in_it_is_left_alone() {
        assert!(!is_due(
            &connection(oauth2(RENEW_BEFORE + Duration::hours(1))).await
        ));
    }

    /// A pasted API key does not expire, so there is nothing here to renew.
    #[tokio::test]
    async fn a_credential_that_does_not_expire_is_never_due() {
        assert!(!is_due(
            &connection(ConnectionSecret::ApiKey { key: "tok".into() }).await
        ));
    }

    /// Only the person who granted it can revive a rejected grant, so asking
    /// the provider again every ten minutes proves nothing.
    #[tokio::test]
    async fn a_connection_needing_reauthorization_is_not_retried() {
        let mut connection = connection(oauth2(Duration::minutes(1))).await;
        connection.status = ConnectionStatus::NeedsReauthorization;

        assert!(!is_due(&connection));
    }
}
