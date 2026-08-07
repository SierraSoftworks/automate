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
use serde_json::{Map, Value};

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

    /// Facts about the linked account which the provider told us and which we
    /// want to keep, but which are neither credentials nor something every
    /// provider has: a GitHub installation's `account_type` — `User` or
    /// `Organization` — being the first of them.
    ///
    /// It exists so that such a fact has somewhere to live on the connection
    /// itself. Without it the only way to keep one was a second record beside
    /// the connection, and two records describing one linked account are two
    /// records that can disagree about it.
    ///
    /// # What must never go in here
    ///
    /// Anything secret. Unlike [`Connection::secret`] this is not sealed — it is
    /// stored in the clear, it is included in a `{:?}`, and it is copied onto
    /// [`ConnectionSummary`] and sent to the browser. A token, a webhook secret,
    /// a signing key: none of those belong here, and the sealed credential is
    /// where they go instead. If you are unsure whether a value is safe to
    /// publish, it is not.
    ///
    /// A map rather than a bare [`serde_json::Value`] so that a connection's
    /// metadata is always an object. That gives merging a single obvious
    /// meaning — see [`ConnectionStore::update_metadata`] — and means no stored
    /// record can turn out to have a number where the rest have fields.
    ///
    /// Defaulted, and omitted when empty, so a connection written before this
    /// existed loads unchanged and does not grow an empty object at rest.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,

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
            metadata: self.metadata.clone(),
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
        self.create_with_metadata(provider, name, account, secret, Map::new())
            .await
    }

    /// Stores a new connection along with what the provider told us about the
    /// account it links to.
    ///
    /// Separate from [`ConnectionStore::create`] rather than an extra argument
    /// on it, because most providers have nothing to record and would only be
    /// passing an empty map through.
    ///
    /// See [`Connection::metadata`] for what may go in there — in particular,
    /// nothing secret.
    pub async fn create_with_metadata(
        &self,
        provider: impl Into<String>,
        name: impl Into<String>,
        account: Option<String>,
        secret: ConnectionSecret,
        metadata: Map<String, Value>,
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
                metadata: metadata.clone(),
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

    /// Records what the provider has told us about a connection's account.
    ///
    /// Merges rather than replaces. Metadata is written by whoever happens to
    /// learn something — the setup wizard, a webhook, a start-up migration — and
    /// none of them holds the whole picture, so a wholesale write would let the
    /// last one to run silently drop what the others had established. Merging is
    /// well defined precisely because [`Connection::metadata`] is a map: the
    /// keys given win, the keys left out are kept.
    ///
    /// Unlike [`ConnectionStore::update_secret`] this leaves the status alone.
    /// Learning that an account is an organisation says nothing about whether
    /// our credential for it still works.
    ///
    /// See [`Connection::metadata`] for what may go in here — in particular,
    /// nothing secret.
    pub async fn update_metadata(
        &self,
        id: ConnectionId,
        metadata: Map<String, Value>,
    ) -> Result<Option<Connection>, human_errors::Error> {
        let Some(mut connection) = self.get(id).await? else {
            return Ok(None);
        };

        connection.metadata.extend(metadata);
        connection.updated_at = Utc::now();

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
    /// # Why removing a GitHub connection is not an uninstall
    ///
    /// A GitHub App installation *is* its connection — there is no second record
    /// beside it — so this is the only thing that has to go. It deliberately
    /// does **not** uninstall the App at GitHub. That revokes our access to
    /// somebody's repositories, it is not undoable from here, and it already has
    /// its own explicit control in the integrations area. Because the App stays
    /// installed, the account can be brought back by running that integration's
    /// setup again — which is what makes removing the connection a recoverable
    /// act rather than a trap.
    pub async fn delete(&self, id: ConnectionId) -> Result<bool, human_errors::Error> {
        if self.get(id).await?.is_none() {
            return Ok(false);
        }

        self.services
            .kv()
            .remove(CONNECTIONS_PARTITION, id.to_string())
            .await?;

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

/// Opens the API key held by one selected connection.
///
/// Workflows carry connection identifiers rather than credentials. Resolving
/// through services scoped to the workflow's tenant ensures that an identifier
/// from another account cannot be used even if it happens to exist there.
pub async fn resolve_api_key(
    id: ConnectionId,
    provider: &str,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<String, human_errors::Error> {
    let store = ConnectionStore::for_services(services);
    let Some(connection) = store.get(id).await? else {
        return Err(human_errors::user(
            format!("The selected {provider} connection ('{id}') no longer exists."),
            &["Link the account again, or select another connection for this workflow."],
        ));
    };

    if connection.provider != provider {
        return Err(human_errors::user(
            format!("The selected connection is not a {provider} connection."),
            &["Select a connection for the service this workflow uses."],
        ));
    }

    if !connection.status.is_usable() {
        return Err(human_errors::user(
            format!("The selected {provider} connection needs attention before it can be used."),
            &["Open Connections and repair or replace its credential."],
        ));
    }

    let ConnectionSecret::ApiKey { key } = store.open(&connection)? else {
        return Err(human_errors::user(
            format!("The selected {provider} connection does not contain an API key."),
            &["Select a personal access token connection instead."],
        ));
    };

    Ok(key)
}

/// Opens the OAuth2 grant held by one selected connection, refreshed and ready
/// to use.
///
/// Returns `Ok(None)` when the grant is dead. A re-authorization reminder has
/// been raised and the connection marked by then, so the caller should complete
/// rather than error: erroring would retry on every schedule against an account
/// nobody can use until it is reconnected.
pub async fn resolve_oauth2_token(
    id: ConnectionId,
    provider: &str,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<Option<OAuth2RefreshToken>, human_errors::Error> {
    let store = ConnectionStore::for_services(services);
    let Some(connection) = store.get(id).await? else {
        return Err(human_errors::user(
            format!("The selected {provider} connection ('{id}') no longer exists."),
            &["Link the account again, or select another connection for this workflow."],
        ));
    };

    if connection.provider != provider {
        return Err(human_errors::user(
            format!("The selected connection is not a {provider} connection."),
            &["Select a connection for the service this workflow uses."],
        ));
    }

    let ConnectionSecret::OAuth2 {
        access_token,
        refresh_token,
        expires_at,
    } = store.open(&connection)?
    else {
        return Err(human_errors::user(
            format!("The selected {provider} connection does not hold an authorization grant."),
            &["Reconnect the account."],
        ));
    };

    let stored = OAuth2RefreshToken::new(access_token, refresh_token, expires_at);

    let Some(token) = crate::web::refresh_or_notify(provider, &stored, services).await? else {
        store
            .set_status(id, ConnectionStatus::NeedsReauthorization)
            .await?;
        return Ok(None);
    };

    // Write the renewed grant back, so the next run starts from it rather than
    // repeating the refresh.
    if token.access_token() != stored.access_token() {
        store
            .update_secret(
                id,
                ConnectionSecret::OAuth2 {
                    access_token: token.access_token().to_string(),
                    refresh_token: token.refresh_token().to_string(),
                    expires_at: token.expires_at(),
                },
            )
            .await?;
    }

    Ok(Some(token))
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
    let mut imported = 0;

    if let Some(api_key) = config
        .connections
        .todoist
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        imported += import_api_key(
            &store,
            crate::publishers::TODOIST_PROVIDER,
            "Todoist",
            api_key,
            &tenant,
        )
        .await?;
    }

    if let Some(api_key) = config
        .connections
        .github
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        imported += import_api_key(
            &store,
            crate::integrations::github_app::GITHUB_PROVIDER,
            "GitHub",
            api_key,
            &tenant,
        )
        .await?;
    }

    if let Some(api_key) = config
        .connections
        .ynab
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        imported += import_api_key(
            &store,
            crate::integrations::ynab::YNAB_PROVIDER,
            "YNAB",
            api_key,
            &tenant,
        )
        .await?;
    }

    Ok(imported)
}

async fn import_api_key<S: Services>(
    store: &ConnectionStore<S>,
    provider: &str,
    name: &str,
    api_key: &str,
    tenant: &TenantId,
) -> Result<usize, human_errors::Error> {
    if store
        .list_for_provider(provider)
        .await?
        .iter()
        .any(|connection| connection.kind == ConnectionKind::ApiKey)
    {
        return Ok(0);
    }

    let connection = store
        .create(
            provider,
            name,
            None,
            ConnectionSecret::ApiKey {
                key: api_key.to_string(),
            },
        )
        .await?;

    warn!(
        connection.id = %connection.id,
        user.account = %tenant,
        "Imported the {name} API key from your configuration file into a connection."
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
    async fn resolving_a_pat_rejects_an_app_connection_from_the_same_provider() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let services = context.tenant(alice());
        let connection = ConnectionStore::for_services(&services)
            .create(
                crate::integrations::github_app::GITHUB_PROVIDER,
                "Example Org",
                Some("example".into()),
                ConnectionSecret::GitHubApp {
                    installation_id: 42,
                },
            )
            .await
            .unwrap();

        let error = resolve_api_key(
            connection.id,
            crate::integrations::github_app::GITHUB_PROVIDER,
            &services,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("does not contain an API key"));
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
    async fn metadata_survives_being_stored_and_read_back() {
        // Metadata is only worth having if it is still there next time; the
        // point of it is to be the one durable record of a fact the provider
        // told us once.
        let store = store().await;

        let created = store
            .create_with_metadata(
                "github",
                "SierraSoftworks",
                Some("SierraSoftworks".into()),
                api_key("tok"),
                Map::from_iter([("account_type".into(), Value::String("Organization".into()))]),
            )
            .await
            .unwrap();

        let loaded = store.get(created.id).await.unwrap().unwrap();
        assert_eq!(
            loaded.metadata.get("account_type"),
            Some(&Value::String("Organization".into()))
        );
    }

    #[tokio::test]
    async fn updating_metadata_adds_to_what_is_there_rather_than_replacing_it() {
        // Several things write metadata and none of them knows the whole
        // picture, so a write that dropped everything it was not told about
        // would quietly lose whatever the others had established.
        let store = store().await;

        let created = store
            .create_with_metadata(
                "github",
                "SierraSoftworks",
                None,
                api_key("tok"),
                Map::from_iter([("account_type".into(), Value::String("User".into()))]),
            )
            .await
            .unwrap();

        let updated = store
            .update_metadata(
                created.id,
                Map::from_iter([("plan".into(), Value::String("free".into()))]),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            updated.metadata.get("account_type"),
            Some(&Value::String("User".into())),
            "a key nobody mentioned must survive the update"
        );
        assert_eq!(
            updated.metadata.get("plan"),
            Some(&Value::String("free".into()))
        );

        // A key that is mentioned is corrected, which is how a stale fact gets
        // put right when the provider changes its mind.
        let corrected = store
            .update_metadata(
                created.id,
                Map::from_iter([("account_type".into(), Value::String("Organization".into()))]),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            corrected.metadata.get("account_type"),
            Some(&Value::String("Organization".into()))
        );
    }

    #[tokio::test]
    async fn a_connection_written_before_metadata_existed_still_loads() {
        // Upgrading must not strand somebody's stored credentials behind a
        // field their records were written without.
        let store = store().await;

        let created = store
            .create("todoist", "Personal", None, api_key("tok"))
            .await
            .unwrap();

        // Exactly what an older agent wrote: every field it knew about, and no
        // `metadata` at all.
        let Value::Object(mut stored) = serde_json::to_value(&created).unwrap() else {
            panic!("a connection should serialise to an object");
        };
        assert!(
            stored.remove("metadata").is_none(),
            "a connection with nothing to record must not write an empty object at rest"
        );

        let loaded: Connection = serde_json::from_value(Value::Object(stored)).unwrap();

        assert!(loaded.metadata.is_empty());

        // The credential has to come back with it, or "it still loads" would be
        // true and useless.
        match store.open(&loaded).unwrap() {
            ConnectionSecret::ApiKey { key } => assert_eq!(key, "tok"),
            other => panic!("expected an api key, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_summary_carries_metadata_but_still_no_credential() {
        // Metadata is the one part of a connection that reaches the browser
        // verbatim, which is exactly why nothing secret may be put in it.
        let store = store().await;

        let created = store
            .create_with_metadata(
                "github",
                "SierraSoftworks",
                Some("SierraSoftworks".into()),
                oauth2(),
                Map::from_iter([("account_type".into(), Value::String("Organization".into()))]),
            )
            .await
            .unwrap();

        let rendered = serde_json::to_string(&created.to_summary()).unwrap();

        assert!(rendered.contains("Organization"), "{rendered}");
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
        assert!(
            store
                .update_metadata(absent, Map::new())
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
    async fn a_configured_github_pat_is_imported_beside_app_installations() {
        let context = AppContext::new_mock(|config| {
            config.connections.github.api_key = Some("legacy-github-pat".into());
        })
        .await
        .unwrap();
        let services = context.tenant(alice());
        let store = ConnectionStore::new(services.clone(), alice());
        store
            .create(
                crate::integrations::github_app::GITHUB_PROVIDER,
                "Example Org",
                Some("example".into()),
                ConnectionSecret::GitHubApp {
                    installation_id: 42,
                },
            )
            .await
            .unwrap();

        assert_eq!(
            import_configured_credentials(services, alice())
                .await
                .unwrap(),
            1
        );

        let connections = store
            .list_for_provider(crate::integrations::github_app::GITHUB_PROVIDER)
            .await
            .unwrap();
        assert_eq!(connections.len(), 2);
        let pat = connections
            .iter()
            .find(|connection| connection.kind == ConnectionKind::ApiKey)
            .unwrap();
        assert!(matches!(
            store.open(pat).unwrap(),
            ConnectionSecret::ApiKey { key } if key == "legacy-github-pat"
        ));
    }

    #[tokio::test]
    async fn a_configured_ynab_token_is_imported_once() {
        let context = AppContext::new_mock(|config| {
            config.connections.ynab.api_key = Some("legacy-ynab-pat".into());
        })
        .await
        .unwrap();

        assert_eq!(
            import_configured_credentials(context.tenant(alice()), alice())
                .await
                .unwrap(),
            1
        );

        let store = ConnectionStore::new(context.tenant(alice()), alice());
        let connections = store
            .list_for_provider(crate::integrations::ynab::YNAB_PROVIDER)
            .await
            .unwrap();
        assert_eq!(connections.len(), 1);
        assert!(matches!(
            store.open(&connections[0]).unwrap(),
            ConnectionSecret::ApiKey { key } if key == "legacy-ynab-pat"
        ));
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
