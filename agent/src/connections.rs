//! Credentials for the services a user has linked.
//!
//! A connection is one account at one external service — a Todoist workspace, a
//! Spotify login, a GitHub App installation — together with whatever is needed
//! to act on that account's behalf.
//!
//! # Why these are records rather than queue messages
//!
//! OAuth grants used to be held as queue payloads, which worked while there was
//! one of everything: the queue was the register of who was connected, and the
//! job that consumed the message was the only thing that needed it. That does
//! not survive contact with several users, each holding several accounts on the
//! same service, and a UI that has to list them without consuming them.
//!
//! # Credentials are not readable by accident
//!
//! [`Connection`] carries its credential sealed and private. Getting at the
//! plaintext means calling [`ConnectionStore::open`], which needs the secret
//! store and reconstructs the binding context from the record's own identity.
//! A sealed credential lifted into another user's record therefore will not
//! open, and a `{:?}` of a connection cannot leak one.

// The API and the integration wizards are wired onto this store in the commits
// that follow; it is written as a complete store rather than grown one endpoint
// at a time.
#![allow(dead_code)]

use chrono::{DateTime, Utc};

use automate_api::{ConnectionId, ConnectionKind, ConnectionStatus, ConnectionSummary};

use crate::crypto::{Sealed, SecretContext};
use crate::db::KeyValueStore;
use crate::prelude::*;

/// The key-value partition holding a tenant's connections.
pub const CONNECTIONS_PARTITION: &str = "connections";

/// How many times to retry a randomly generated identifier before giving up.
///
/// Identifiers are drawn from a space of four million per tenant and a tenant
/// holds a handful of connections, so reaching even the second attempt is
/// essentially impossible; this exists so that a bug cannot become an infinite
/// loop.
const ID_ATTEMPTS: usize = 8;

/// The credential a connection holds.
///
/// Only ever seen in plaintext between [`ConnectionStore::open`] and the client
/// that uses it; at rest it is the ciphertext inside [`Connection::secret`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionSecret {
    /// An OAuth 2.0 grant. The refresh token is the durable part; the access
    /// token is a cache of the last exchange.
    #[serde(rename = "oauth2")]
    OAuth2 {
        access_token: String,
        refresh_token: String,
        expires_at: DateTime<Utc>,
    },

    /// A token the user pasted in.
    #[serde(rename = "api_key")]
    ApiKey { key: String },

    /// A GitHub App installation, which authenticates with the app's own key
    /// rather than anything user-specific, so only the installation is stored.
    #[serde(rename = "github_app")]
    GitHubApp { installation_id: u64 },
}

impl ConnectionSecret {
    /// Which kind of connection this credential belongs to.
    pub fn kind(&self) -> ConnectionKind {
        match self {
            Self::OAuth2 { .. } => ConnectionKind::OAuth2,
            Self::ApiKey { .. } => ConnectionKind::ApiKey,
            Self::GitHubApp { .. } => ConnectionKind::GitHubApp,
        }
    }

    /// When this credential stops working, for the kinds that expire.
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::OAuth2 { expires_at, .. } => Some(*expires_at),
            _ => None,
        }
    }
}

/// A linked account, with its credential sealed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnectionId,

    /// The service this connects to, matching an `[oauth2.*]` key or a built-in
    /// provider name.
    pub provider: String,

    /// Kept alongside the sealed credential so a connection can be listed,
    /// filtered and displayed without decrypting anything.
    pub kind: ConnectionKind,

    /// What to call this connection.
    pub name: String,

    /// The account at the provider, used to recognise a repeat authorisation of
    /// the same account rather than creating a duplicate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,

    #[serde(default)]
    pub status: ConnectionStatus,

    /// Denormalised from the credential so expiry can be shown without
    /// decrypting it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    /// The sealed credential.
    ///
    /// Private, so the only route to the plaintext is [`ConnectionStore::open`],
    /// which supplies the binding context this was sealed against.
    secret: Sealed,
}

impl Connection {
    /// The view of this connection that is safe to send to a browser.
    pub fn to_summary(&self) -> ConnectionSummary {
        ConnectionSummary {
            id: self.id,
            provider: self.provider.clone(),
            kind: self.kind,
            name: self.name.clone(),
            account: self.account.clone(),
            status: self.status,
            expires_at: self.expires_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Reads and writes one tenant's connections.
pub struct ConnectionStore<S: Services> {
    services: S,
    tenant: TenantId,
}

impl<S: Services> ConnectionStore<S> {
    /// Wraps services already scoped to `tenant`.
    pub fn new(services: S, tenant: TenantId) -> Self {
        Self { services, tenant }
    }

    /// Wraps services, taking the account from them.
    pub fn for_services(services: S) -> Self {
        let tenant = services.tenant().clone();

        Self { services, tenant }
    }

    fn context(&self, id: ConnectionId) -> SecretContext<'_> {
        SecretContext::Connection {
            tenant: self.tenant.as_str(),
            connection: id,
        }
    }

    /// Looks up a single connection.
    pub async fn get(&self, id: ConnectionId) -> Result<Option<Connection>, human_errors::Error> {
        self.services
            .kv()
            .get(CONNECTIONS_PARTITION, id.to_string())
            .await
    }

    /// Every connection this tenant holds, most recently created first.
    pub async fn list(&self) -> Result<Vec<Connection>, human_errors::Error> {
        let mut connections: Vec<Connection> = self
            .services
            .kv()
            .list::<Connection>(CONNECTIONS_PARTITION)
            .await?
            .into_iter()
            .map(|(_, connection)| connection)
            .collect();

        connections.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));

        Ok(connections)
    }

    /// The connections linking to one service.
    pub async fn list_for_provider(
        &self,
        provider: &str,
    ) -> Result<Vec<Connection>, human_errors::Error> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|connection| connection.provider == provider)
            .collect())
    }

    /// Finds an existing connection to the same account at the same service.
    ///
    /// Used when an authorisation completes, so that re-authorising an account
    /// refreshes the connection already there instead of leaving the user with
    /// two indistinguishable entries.
    pub async fn find_by_account(
        &self,
        provider: &str,
        account: &str,
    ) -> Result<Option<Connection>, human_errors::Error> {
        Ok(self
            .list_for_provider(provider)
            .await?
            .into_iter()
            .find(|connection| connection.account.as_deref() == Some(account)))
    }

    /// Stores a new connection, sealing its credential.
    pub async fn create(
        &self,
        provider: impl Into<String>,
        name: impl Into<String>,
        account: Option<String>,
        secret: ConnectionSecret,
    ) -> Result<Connection, human_errors::Error> {
        let now = Utc::now();
        let provider = provider.into();
        let name = name.into();

        for _ in 0..ID_ATTEMPTS {
            let id = ConnectionId::from_entropy(rand::random());

            let connection = Connection {
                id,
                provider: provider.clone(),
                kind: secret.kind(),
                name: name.clone(),
                account: account.clone(),
                status: ConnectionStatus::Ok,
                expires_at: secret.expires_at(),
                created_at: now,
                updated_at: now,
                secret: self
                    .services
                    .secrets()
                    .seal_json(&secret, self.context(id))?,
            };

            // Insert rather than set, so a collision is reported and retried
            // instead of overwriting somebody's working connection.
            if self
                .services
                .kv()
                .insert(CONNECTIONS_PARTITION, id.to_string(), connection.clone())
                .await?
            {
                return Ok(connection);
            }

            warn!(connection.id = %id, "Generated a connection identifier that was already taken; retrying.");
        }

        Err(human_errors::system(
            "We could not allocate an identifier for this connection.",
            &["This is unexpected; please report it with the surrounding log entries."],
        ))
    }

    /// Decrypts a connection's credential.
    pub fn open(&self, connection: &Connection) -> Result<ConnectionSecret, human_errors::Error> {
        let secret: ConnectionSecret = self
            .services
            .secrets()
            .open_json(&connection.secret, self.context(connection.id))?;

        // The plaintext carries its own tag, so a record whose visible kind
        // disagrees with what it actually holds has been tampered with or
        // written by something that got it wrong.
        if secret.kind() != connection.kind {
            return Err(human_errors::system(
                "A stored connection does not hold the kind of credential it claims to.",
                &["Remove and recreate this connection."],
            ));
        }

        Ok(secret)
    }

    /// Replaces a connection's credential, as after refreshing an OAuth token.
    ///
    /// Clears any error status, since a credential we have just obtained is by
    /// definition the current one.
    pub async fn update_secret(
        &self,
        id: ConnectionId,
        secret: ConnectionSecret,
    ) -> Result<Option<Connection>, human_errors::Error> {
        let Some(mut connection) = self.get(id).await? else {
            return Ok(None);
        };

        connection.kind = secret.kind();
        connection.expires_at = secret.expires_at();
        connection.status = ConnectionStatus::Ok;
        connection.updated_at = Utc::now();
        connection.secret = self
            .services
            .secrets()
            .seal_json(&secret, self.context(id))?;

        self.put(&connection).await?;

        Ok(Some(connection))
    }

    /// Records that a connection has stopped working.
    pub async fn set_status(
        &self,
        id: ConnectionId,
        status: ConnectionStatus,
    ) -> Result<Option<Connection>, human_errors::Error> {
        let Some(mut connection) = self.get(id).await? else {
            return Ok(None);
        };

        connection.status = status;
        connection.updated_at = Utc::now();

        self.put(&connection).await?;

        Ok(Some(connection))
    }

    /// Renames a connection.
    pub async fn rename(
        &self,
        id: ConnectionId,
        name: impl Into<String>,
    ) -> Result<Option<Connection>, human_errors::Error> {
        let Some(mut connection) = self.get(id).await? else {
            return Ok(None);
        };

        connection.name = name.into();
        connection.updated_at = Utc::now();

        self.put(&connection).await?;

        Ok(Some(connection))
    }

    /// Removes a connection.
    ///
    /// Workflows referring to it are left alone: they will report a missing
    /// connection, which is a clearer thing for the user to see than having
    /// silently disappeared along with the credential.
    ///
    /// # Why a GitHub connection takes its installation record with it
    ///
    /// A GitHub App connection is written together with an entry in the
    /// installation registry, by
    /// [`crate::integrations::github_app::record_installation`]. Removing only
    /// one of the pair would leave the integrations page still listing the
    /// account as connected while the connections page and every picker say it
    /// is not, and the person would have no way to tell which was right. So both
    /// go.
    ///
    /// It deliberately does **not** uninstall the App at GitHub. That revokes
    /// our access to somebody's repositories, it is not undoable from here, and
    /// it already has its own explicit control in the integrations area. Because
    /// the App stays installed, the account can be brought back by running that
    /// integration's setup again — which is what makes removing the connection a
    /// recoverable act rather than a trap.
    ///
    /// Done here rather than in the HTTP handler so that it holds for every
    /// caller, including
    /// [`crate::integrations::github_app::forget_installation`].
    pub async fn delete(&self, id: ConnectionId) -> Result<bool, human_errors::Error> {
        let Some(connection) = self.get(id).await? else {
            return Ok(false);
        };

        self.services
            .kv()
            .remove(CONNECTIONS_PARTITION, id.to_string())
            .await?;

        if connection.provider == crate::integrations::github_app::GITHUB_PROVIDER
            && let Some(account) = connection.account
        {
            self.services
                .kv()
                .remove(
                    crate::integrations::github_app::INSTALLATIONS_PARTITION,
                    account,
                )
                .await?;
        }

        Ok(true)
    }

    async fn put(&self, connection: &Connection) -> Result<(), human_errors::Error> {
        self.services
            .kv()
            .set(
                CONNECTIONS_PARTITION,
                connection.id.to_string(),
                connection.clone(),
            )
            .await
    }
}

/// Moves credentials out of the configuration file and into connections.
///
/// An installation configured before connections existed keeps its Todoist key
/// in `config.toml`. Rather than making the operator recreate it by hand — and
/// having their workflows quietly stop publishing until they do — it is imported
/// once, into the account a single-user installation runs as.
///
/// Idempotent: an account that already has a connection to the service is left
/// alone, so this cannot overwrite a credential somebody has since replaced
/// through the UI.
pub async fn import_configured_credentials<S: Services>(
    services: S,
    tenant: TenantId,
) -> Result<usize, human_errors::Error> {
    let config = services.config();
    let store = ConnectionStore::new(services, tenant.clone());

    let Some(api_key) = config
        .connections
        .todoist
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    else {
        return Ok(0);
    };

    if !store
        .list_for_provider(crate::publishers::TODOIST_PROVIDER)
        .await?
        .is_empty()
    {
        return Ok(0);
    }

    let connection = store
        .create(
            crate::publishers::TODOIST_PROVIDER,
            "Todoist",
            None,
            ConnectionSecret::ApiKey {
                key: api_key.to_string(),
            },
        )
        .await?;

    warn!(
        connection.id = %connection.id,
        user.account = %tenant,
        "Imported the Todoist API key from your configuration file into a connection. \
         You can now remove [connections.todoist] from config.toml."
    );

    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{AppContext, ServicesContainer};

    type TestStore = ConnectionStore<ServicesContainer<crate::db::TenantDb>>;

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    async fn store() -> TestStore {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        ConnectionStore::new(context.tenant(alice()), alice())
    }

    fn api_key(key: &str) -> ConnectionSecret {
        ConnectionSecret::ApiKey { key: key.into() }
    }

    fn oauth2() -> ConnectionSecret {
        // Distinctive values, so a test asserting they are absent cannot be
        // satisfied - or defeated - by an unrelated substring.
        ConnectionSecret::OAuth2 {
            access_token: "access-tYqR9".into(),
            refresh_token: "refresh-Kx3Lm".into(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
        }
    }

    #[tokio::test]
    async fn a_created_connection_can_be_read_back_with_its_credential() {
        let store = store().await;

        let created = store
            .create(
                "todoist",
                "Personal",
                Some("alice@example.com".into()),
                api_key("tok"),
            )
            .await
            .unwrap();

        assert_eq!(created.kind, ConnectionKind::ApiKey);
        assert_eq!(created.status, ConnectionStatus::Ok);
        assert_eq!(created.expires_at, None);

        let loaded = store.get(created.id).await.unwrap().unwrap();
        match store.open(&loaded).unwrap() {
            ConnectionSecret::ApiKey { key } => assert_eq!(key, "tok"),
            other => panic!("expected an api key, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_credential_is_not_readable_from_the_stored_record() {
        let store = store().await;

        let created = store
            .create("todoist", "Personal", None, api_key("super-secret"))
            .await
            .unwrap();

        let stored = serde_json::to_string(&created).unwrap();
        assert!(
            !stored.contains("super-secret"),
            "the credential must not be stored in the clear: {stored}"
        );
        assert!(
            !format!("{created:?}").contains("super-secret"),
            "a debug rendering must not reveal the credential"
        );
    }

    #[tokio::test]
    async fn a_summary_carries_no_credential_and_survives_the_wire() {
        let store = store().await;

        let created = store
            .create("spotify", "Music", Some("alice".into()), oauth2())
            .await
            .unwrap();

        let summary = created.to_summary();
        assert_eq!(summary.kind, ConnectionKind::OAuth2);
        assert!(summary.expires_at.is_some(), "an OAuth grant expires");

        let rendered = serde_json::to_string(&summary).unwrap();
        assert!(!rendered.contains("refresh-Kx3Lm"), "{rendered}");
        assert!(!rendered.contains("access-tYqR9"), "{rendered}");
    }

    #[tokio::test]
    async fn a_credential_cannot_be_opened_from_another_tenants_store() {
        // The property the sealing context exists for: lifting a stored
        // connection into somebody else's namespace must not yield a usable
        // credential.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let mallory = TenantId::new("mallory").unwrap();

        let alices = ConnectionStore::new(context.tenant(alice()), alice());
        let connection = alices
            .create("todoist", "Personal", None, api_key("tok"))
            .await
            .unwrap();

        let mallorys = ConnectionStore::new(context.tenant(mallory.clone()), mallory);
        assert!(
            mallorys.open(&connection).is_err(),
            "a credential sealed for one account must not open under another"
        );
    }

    #[tokio::test]
    async fn a_credential_cannot_be_moved_to_another_connection() {
        let store = store().await;

        let source = store
            .create("todoist", "Personal", None, api_key("tok"))
            .await
            .unwrap();
        let mut target = store
            .create("todoist", "Work", None, api_key("other"))
            .await
            .unwrap();

        // Graft one connection's sealed credential onto another's record.
        target.secret = source.secret.clone();

        assert!(
            store.open(&target).is_err(),
            "a credential must not open under a different connection's identity"
        );
    }

    #[tokio::test]
    async fn refreshing_a_credential_updates_the_expiry_and_clears_the_error() {
        let store = store().await;

        let created = store
            .create("spotify", "Music", None, oauth2())
            .await
            .unwrap();
        store
            .set_status(created.id, ConnectionStatus::NeedsReauthorization)
            .await
            .unwrap();

        let renewed = ConnectionSecret::OAuth2 {
            access_token: "access-renewed".into(),
            refresh_token: "refresh-renewed".into(),
            expires_at: Utc::now() + chrono::Duration::hours(4),
        };
        let updated = store
            .update_secret(created.id, renewed)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(updated.status, ConnectionStatus::Ok);
        assert!(updated.expires_at.unwrap() > created.expires_at.unwrap());

        match store.open(&updated).unwrap() {
            ConnectionSecret::OAuth2 { refresh_token, .. } => {
                assert_eq!(refresh_token, "refresh-renewed")
            }
            other => panic!("expected an OAuth grant, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_record_claiming_the_wrong_kind_of_credential_is_refused() {
        // The visible kind drives what the UI offers and what a workflow
        // expects, so a record whose plaintext disagrees with it has been
        // tampered with.
        let store = store().await;

        let mut connection = store
            .create("todoist", "Personal", None, api_key("tok"))
            .await
            .unwrap();
        connection.kind = ConnectionKind::OAuth2;

        assert!(store.open(&connection).is_err());
    }

    #[tokio::test]
    async fn re_authorising_an_account_can_be_recognised_rather_than_duplicated() {
        let store = store().await;

        store
            .create("spotify", "Music", Some("alice".into()), oauth2())
            .await
            .unwrap();

        assert!(
            store
                .find_by_account("spotify", "alice")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .find_by_account("spotify", "someone-else")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .find_by_account("todoist", "alice")
                .await
                .unwrap()
                .is_none(),
            "accounts must not be matched across different services"
        );
    }

    #[tokio::test]
    async fn connections_can_be_listed_per_service() {
        let store = store().await;

        for (provider, name) in [
            ("todoist", "Personal"),
            ("todoist", "Work"),
            ("spotify", "Music"),
        ] {
            store
                .create(provider, name, None, api_key("k"))
                .await
                .unwrap();
        }

        assert_eq!(store.list().await.unwrap().len(), 3);
        assert_eq!(store.list_for_provider("todoist").await.unwrap().len(), 2);
        assert_eq!(store.list_for_provider("ynab").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn removing_a_connection_reports_whether_there_was_one() {
        let store = store().await;

        let created = store
            .create("todoist", "Personal", None, api_key("tok"))
            .await
            .unwrap();

        assert!(store.delete(created.id).await.unwrap());
        assert!(store.get(created.id).await.unwrap().is_none());
        assert!(
            !store.delete(created.id).await.unwrap(),
            "deleting an absent connection should report that there was nothing to remove"
        );
    }

    #[tokio::test]
    async fn operations_on_an_unknown_connection_report_absence_rather_than_failing() {
        let store = store().await;
        let absent = ConnectionId::from_entropy(12345);

        assert!(store.get(absent).await.unwrap().is_none());
        assert!(store.rename(absent, "x").await.unwrap().is_none());
        assert!(
            store
                .set_status(absent, ConnectionStatus::Error)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .update_secret(absent, api_key("k"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_configured_todoist_key_is_imported_once() {
        let context = AppContext::new_mock(|config| {
            config.connections.todoist.api_key = Some("legacy-tok".into());
        })
        .await
        .unwrap();

        let imported = import_configured_credentials(context.tenant(alice()), alice())
            .await
            .unwrap();
        assert_eq!(imported, 1);

        let store = ConnectionStore::new(context.tenant(alice()), alice());
        let connections = store.list_for_provider("todoist").await.unwrap();
        assert_eq!(connections.len(), 1);

        match store.open(&connections[0]).unwrap() {
            ConnectionSecret::ApiKey { key } => assert_eq!(key, "legacy-tok"),
            other => panic!("expected an api key, got {other:?}"),
        }

        // Running again must not add a second copy, nor overwrite one the user
        // has since replaced through the UI.
        assert_eq!(
            import_configured_credentials(context.tenant(alice()), alice())
                .await
                .unwrap(),
            0
        );
        assert_eq!(store.list_for_provider("todoist").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn nothing_is_imported_when_no_credential_was_configured() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        assert_eq!(
            import_configured_credentials(context.tenant(alice()), alice())
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn one_tenants_connections_are_invisible_to_another() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let bob = TenantId::new("bob").unwrap();

        ConnectionStore::new(context.tenant(alice()), alice())
            .create("todoist", "Personal", None, api_key("tok"))
            .await
            .unwrap();

        let bobs = ConnectionStore::new(context.tenant(bob.clone()), bob);
        assert!(bobs.list().await.unwrap().is_empty());
    }
}
