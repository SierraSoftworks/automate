//! Routing an inbound webhook delivery to the workflow that asked for it.
//!
//! A delivery arrives with nothing but a URL. There is no session, no account,
//! and nothing to ask — whatever service is calling knows only what it was told
//! to call. So the URL has to carry the answer, and the token in it is what
//! identifies both the workflow and the account it belongs to.
//!
//! # The index holds no secret
//!
//! Lookups happen before a tenant is known, so the index has to live somewhere
//! that belongs to nobody — [`TenantId::system`], which the tenant module has
//! always described as the home for exactly this. It is keyed by the *hash* of a
//! token rather than the token, so a copy of the index is not a set of working
//! webhook URLs. The token itself is sealed inside the workflow's own record,
//! where it can be shown to its owner again and nowhere else.
//!
//! # The index is a hint; the record is the truth
//!
//! Resolving checks the token against the one sealed in the workflow it landed
//! on, rather than trusting the index to have pointed at the right place. The
//! index is derived data and could be stale or tampered with; the record is what
//! the owner actually saved. The comparison is constant-time, and a mismatch is
//! refused in exactly the same words as a token nobody has ever issued.

use automate_api::{TenantId, WebhookToken, WorkflowId};
use human_errors::Error;

use crate::crypto::{Sealed, SecretContext};
use crate::db::KeyValueStore;
use crate::prelude::*;

/// The partition in the system tenant holding the token index.
pub const WEBHOOK_INDEX_PARTITION: &str = "webhook-tokens";

/// Where a token points.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebhookRoute {
    /// The account that owns the workflow.
    pub tenant: TenantId,

    /// The workflow to run.
    pub workflow: WorkflowId,
}

/// Mints a new token.
///
/// Lives here rather than on [`WebhookToken`] because that type compiles to
/// WebAssembly for the browser, which has no business holding a randomness
/// source or minting a credential the agent will have to trust.
pub fn mint() -> WebhookToken {
    WebhookToken::from_bytes(rand::random())
}

/// The key a token is filed under.
///
/// A hash rather than the token, so that being able to read the index is not the
/// same as being able to post to every webhook in the installation.
pub fn index_key(token: &WebhookToken) -> String {
    sha256::digest(token.as_bytes().as_slice())
}

/// Seals a token so it can be kept in a workflow record.
///
/// Bound to the tenant and workflow it belongs to, so a sealed token lifted into
/// another record will not open — the same protection connections have.
pub fn seal(
    secrets: &crate::crypto::SecretStore,
    token: &WebhookToken,
    tenant: &TenantId,
    workflow: WorkflowId,
) -> Result<Sealed, Error> {
    secrets.seal(
        token.to_string().as_bytes(),
        SecretContext::WebhookSecret {
            tenant: tenant.as_str(),
            workflow,
        },
    )
}

/// Reads a token back out of a workflow record.
pub fn open(
    secrets: &crate::crypto::SecretStore,
    sealed: &Sealed,
    tenant: &TenantId,
    workflow: WorkflowId,
) -> Result<WebhookToken, Error> {
    let plaintext = secrets.open(
        sealed,
        SecretContext::WebhookSecret {
            tenant: tenant.as_str(),
            workflow,
        },
    )?;

    let encoded = String::from_utf8(plaintext).wrap_system_err(
        "The stored webhook token could not be read.",
        &["Rotate this workflow's webhook URL to replace it."],
    )?;

    encoded.parse::<WebhookToken>().wrap_system_err(
        "The stored webhook token could not be read.",
        &["Rotate this workflow's webhook URL to replace it."],
    )
}

/// Maintains the installation-wide index of webhook tokens.
pub struct WebhookIndex<S> {
    /// Scoped to the system tenant, because a delivery has to be resolved before
    /// anybody knows whose it is.
    services: S,
}

impl<S: Services> WebhookIndex<S> {
    pub fn new(system_services: S) -> Self {
        Self {
            services: system_services,
        }
    }

    /// Files a token against the workflow it belongs to.
    pub async fn insert(&self, token: &WebhookToken, route: WebhookRoute) -> Result<(), Error> {
        self.services
            .kv()
            .set(WEBHOOK_INDEX_PARTITION, index_key(token), route)
            .await
    }

    /// Removes a token, so a URL that has been rotated or deleted stops working.
    pub async fn remove(&self, token: &WebhookToken) -> Result<(), Error> {
        self.services
            .kv()
            .remove(WEBHOOK_INDEX_PARTITION, index_key(token))
            .await
    }

    /// Finds where a token points, if anywhere.
    pub async fn lookup(&self, token: &WebhookToken) -> Result<Option<WebhookRoute>, Error> {
        self.services
            .kv()
            .get(WEBHOOK_INDEX_PARTITION, index_key(token))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ServicesContainer;

    fn token(seed: u8) -> WebhookToken {
        WebhookToken::from_bytes([seed; 16])
    }

    fn route() -> WebhookRoute {
        WebhookRoute {
            tenant: TenantId::new("alice").unwrap(),
            workflow: WorkflowId::from_entropy(1),
        }
    }

    #[tokio::test]
    async fn a_filed_token_points_at_the_workflow_it_was_filed_against() {
        let services = ServicesContainer::new_mock().await.unwrap();
        let index = WebhookIndex::new(&services);

        index.insert(&token(1), route()).await.unwrap();

        assert_eq!(index.lookup(&token(1)).await.unwrap(), Some(route()));
    }

    #[tokio::test]
    async fn a_token_nobody_filed_points_nowhere() {
        let services = ServicesContainer::new_mock().await.unwrap();
        let index = WebhookIndex::new(&services);

        assert_eq!(index.lookup(&token(9)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_removed_token_stops_working() {
        let services = ServicesContainer::new_mock().await.unwrap();
        let index = WebhookIndex::new(&services);

        index.insert(&token(1), route()).await.unwrap();
        index.remove(&token(1)).await.unwrap();

        assert_eq!(
            index.lookup(&token(1)).await.unwrap(),
            None,
            "a rotated or deleted URL must stop being accepted immediately",
        );
    }

    #[tokio::test]
    async fn the_index_does_not_contain_the_tokens_it_files() {
        // Someone who can read the database should not thereby hold a working
        // webhook URL for every workflow in the installation.
        let services = ServicesContainer::new_mock().await.unwrap();
        let index = WebhookIndex::new(&services);
        let token = token(7);

        index.insert(&token, route()).await.unwrap();

        let stored: Vec<(String, WebhookRoute)> = crate::prelude::Services::kv(&services)
            .list(WEBHOOK_INDEX_PARTITION)
            .await
            .unwrap();

        assert_eq!(stored.len(), 1);
        assert_ne!(
            stored[0].0,
            token.to_string(),
            "the index is keyed by the token itself, so reading it yields working URLs",
        );
    }

    #[test]
    fn a_sealed_token_will_not_open_in_another_workflows_record() {
        // The same protection a connection's credential has: a token lifted from
        // one record into another is not readable there.
        let secrets = crate::crypto::SecretStore::ephemeral();
        let tenant = TenantId::new("alice").unwrap();
        let (mine, theirs) = (WorkflowId::from_entropy(1), WorkflowId::from_entropy(2));

        let sealed = seal(&secrets, &token(3), &tenant, mine).unwrap();

        assert_eq!(open(&secrets, &sealed, &tenant, mine).unwrap(), token(3));
        assert!(open(&secrets, &sealed, &tenant, theirs).is_err());
    }

    #[test]
    fn a_sealed_token_will_not_open_for_another_account() {
        let secrets = crate::crypto::SecretStore::ephemeral();
        let workflow = WorkflowId::from_entropy(1);
        let (alice, bob) = (
            TenantId::new("alice").unwrap(),
            TenantId::new("bob").unwrap(),
        );

        let sealed = seal(&secrets, &token(3), &alice, workflow).unwrap();

        assert!(open(&secrets, &sealed, &bob, workflow).is_err());
    }
}
