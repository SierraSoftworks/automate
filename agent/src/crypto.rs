//! Encryption for the credentials Automate holds on behalf of its users.
//!
//! In a single-tenant install the operator owns every secret in the database,
//! so storing them in plaintext costs them nothing they did not already have.
//! Once the agent holds *other people's* Todoist tokens, GitHub credentials and
//! webhook signing secrets, that changes: a leaked database file should not be
//! a leak of every tenant's accounts.
//!
//! # Shape of the design
//!
//! Secrets are sealed with AES-256-GCM into a self-describing [`Sealed`]
//! envelope which is stored as an ordinary JSON value, so it travels through
//! the existing key-value store without any special handling.
//!
//! Two deliberate choices are worth calling out.
//!
//! **Sealing is explicit, not automatic.** It would be more ergonomic to hide
//! encryption inside a `serde` implementation so that any field could be
//! declared secret and forgotten about. We do not, because `serde` cannot carry
//! the context needed for the next property, and because an explicit
//! [`SecretStore::open`] call leaves every point where a secret is exposed
//! visible to a reader and greppable in review.
//!
//! **Every ciphertext is bound to the row that holds it.** The [`SecretContext`]
//! is passed as GCM additional authenticated data and is *not* stored in the
//! envelope — it is reconstructed from the row's own identity at decryption
//! time. An attacker who can write to the database but does not hold the key
//! therefore cannot copy one tenant's sealed Todoist token into another
//! tenant's connection row and have the agent decrypt and use it: the
//! reconstructed context differs, the authentication tag fails, and the open
//! is rejected.
//!
//! # Key management
//!
//! The active key comes from configuration (and so, in practice, from an
//! environment variable or secret manager). Where none is configured we
//! generate one into a file beside the database, which keeps existing
//! single-tenant installs upgrading without ceremony while still moving the key
//! off the backup path of the database itself.
//!
//! Each envelope records which key sealed it, so rotation is a matter of moving
//! the old key into `previous_secret_keys` and letting values re-seal under the
//! new key as they are written. Decryption picks the right key by id rather
//! than trying each in turn.

// This module is a self-contained capability whose surface is consumed
// incrementally by the connection and webhook stores, in the same spirit as the
// storage traits in `crate::db`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, Generate, Key, KeyInit, Nonce, Payload};
use base64::Engine;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use automate_api::{ConnectionId, WorkflowId};
use human_errors::ResultExt;

use crate::prelude::*;

/// The base64 alphabet used for every encoded value in this module.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The length of an AES-256 key, in bytes.
const KEY_BYTES: usize = 32;

/// The version stamped into new envelopes, so the format can change later
/// without leaving us unable to tell which rules an old value was written under.
const ENVELOPE_VERSION: u8 = 1;

/// The filename suffix of the generated key file, appended to the database path.
const KEY_FILE_SUFFIX: &str = ".key";

/// A short, non-secret fingerprint identifying which key sealed a value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyId([u8; 4]);

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({self})")
    }
}

/// A 256-bit symmetric key.
///
/// The bytes are zeroed when the key is dropped, and neither [`fmt::Debug`] nor
/// any other formatting impl will render them — only the non-secret
/// [`KeyId`].
pub struct SecretKey {
    bytes: [u8; KEY_BYTES],
}

impl SecretKey {
    /// Generates a new key from the operating system's randomness source.
    pub fn generate() -> Self {
        let key = Key::<Aes256Gcm>::generate();
        let mut bytes = [0u8; KEY_BYTES];
        bytes.copy_from_slice(key.as_slice());

        Self { bytes }
    }

    /// Parses a key from its base64 or hexadecimal encoding.
    ///
    /// Both alphabets are accepted because operators paste keys from a variety
    /// of sources, and guessing wrong is a confusing failure to debug.
    pub fn from_encoded(encoded: &str) -> Result<Self, human_errors::Error> {
        let encoded = encoded.trim();

        // The config loader leaves an unresolved `${{ env.X }}` expression in
        // place verbatim rather than failing, so this is the most likely way
        // for a key to be malformed and deserves its own diagnosis.
        if encoded.contains("${{") {
            return Err(human_errors::user(
                "Your encryption key still contains an unresolved '${{ ... }}' expression.",
                &[
                    "Check that the environment variable it refers to is set in your environment or .env file.",
                    "Remember that the agent reads .env from the path given by --env, which defaults to '.env'.",
                ],
            ));
        }

        if encoded.is_empty() {
            return Err(human_errors::user(
                "Your encryption key is empty.",
                &[
                    "Set 'secret_key' under [web.auth] to a 32-byte key, or remove it to have one generated for you.",
                    "You can generate one with: openssl rand -base64 32",
                ],
            ));
        }

        let decoded = decode_key_bytes(encoded)?;

        if decoded.len() != KEY_BYTES {
            return Err(human_errors::user(
                format!(
                    "Your encryption key decodes to {} bytes, but AES-256 requires exactly {KEY_BYTES}.",
                    decoded.len()
                ),
                &[
                    "Generate a key of the correct length with: openssl rand -base64 32",
                    "Check that the value was not truncated when it was copied or stored.",
                ],
            ));
        }

        let mut bytes = [0u8; KEY_BYTES];
        bytes.copy_from_slice(&decoded);

        Ok(Self { bytes })
    }

    /// Encodes the key for storage in configuration or a key file.
    pub fn to_encoded(&self) -> String {
        B64.encode(self.bytes)
    }

    /// A short fingerprint of the key, safe to record in envelopes and logs.
    ///
    /// This is a hash of the key rather than a slice of it, so publishing it
    /// reveals nothing about the key material.
    pub fn id(&self) -> KeyId {
        let digest = Sha256::new()
            .chain_update(b"automate/secret-key-id/v1")
            .chain_update(self.bytes)
            .finalize();

        let mut id = [0u8; 4];
        id.copy_from_slice(&digest[..4]);

        KeyId(id)
    }

    fn cipher(&self) -> Aes256Gcm {
        // The byte count is fixed by the type, so this cannot fail.
        Aes256Gcm::new_from_slice(&self.bytes).expect("an AES-256 key is always 32 bytes")
    }
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey({})", self.id())
    }
}

/// Decodes key material from either base64 or hexadecimal.
fn decode_key_bytes(encoded: &str) -> Result<Vec<u8>, human_errors::Error> {
    // A 32-byte key is 64 hex characters. Anything that length made up purely
    // of hex digits is unambiguously hex, since base64 of 32 bytes is 43-44
    // characters.
    if encoded.len() == KEY_BYTES * 2 && encoded.bytes().all(|b| b.is_ascii_hexdigit()) {
        return hex::decode(encoded).wrap_user_err(
            "Your encryption key could not be decoded as hexadecimal.",
            &["Check that the value contains only hexadecimal digits."],
        );
    }

    // Accept both the URL-safe and standard base64 alphabets, with or without
    // padding, since `openssl rand -base64 32` emits standard-alphabet output.
    for engine in [
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
    ] {
        if let Ok(decoded) = engine.decode(encoded.trim_end_matches('=')) {
            return Ok(decoded);
        }
    }

    Err(human_errors::user(
        "Your encryption key could not be decoded as base64 or hexadecimal.",
        &[
            "Generate a valid key with: openssl rand -base64 32",
            "Check that the value was not wrapped across lines or otherwise altered.",
        ],
    ))
}

/// Identifies the row a sealed value belongs to.
///
/// The rendered form is used as GCM additional authenticated data, which binds
/// each ciphertext to its location. Variants exist for every kind of secret we
/// store so that the sealing and opening sites cannot drift apart: both name
/// the same variant rather than assembling a string independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretContext<'a> {
    /// The credential held by a connection to an external service.
    Connection {
        tenant: &'a str,
        connection: ConnectionId,
    },

    /// The signing secret a webhook-triggered workflow verifies deliveries with.
    WebhookSecret {
        tenant: &'a str,
        workflow: WorkflowId,
    },
}

impl fmt::Display for SecretContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection { tenant, connection } => {
                write!(f, "automate/v1/connection/{tenant}/{connection}")
            }
            Self::WebhookSecret { tenant, workflow } => {
                write!(f, "automate/v1/webhook-secret/{tenant}/{workflow}")
            }
        }
    }
}

/// An encrypted value, together with the metadata needed to decrypt it.
///
/// This is stored as an ordinary JSON object, so it can live inside any record
/// the key-value store already holds. The context the value was bound to is
/// deliberately absent: it is reconstructed from the surrounding record, which
/// is what makes relocating a ciphertext detectable.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sealed {
    /// Envelope format version.
    v: u8,

    /// The [`KeyId`] of the key this value was sealed with.
    kid: String,

    /// The per-message nonce.
    n: String,

    /// The ciphertext, with the GCM authentication tag appended.
    c: String,
}

impl fmt::Debug for Sealed {
    /// Never renders the ciphertext, so that a stray `{:?}` in a log line
    /// cannot dump the encrypted store into an observability backend.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sealed(v{}, key {})", self.v, self.kid)
    }
}

/// Seals and opens the secrets held on behalf of users.
///
/// Obtained from [`crate::services::Services::secrets`] rather than a global,
/// so that tests can supply their own key and so the dependency is visible in
/// the signature of anything that touches secrets.
pub struct SecretStore {
    active: SecretKey,
    keys: HashMap<KeyId, SecretKey>,
}

impl SecretStore {
    /// Builds a store which seals with `active` and can open values sealed with
    /// `active` or any of `previous`.
    pub fn new(active: SecretKey, previous: Vec<SecretKey>) -> Self {
        let mut keys = HashMap::new();

        for key in previous {
            keys.insert(key.id(), key);
        }

        keys.insert(
            active.id(),
            SecretKey {
                bytes: active.bytes,
            },
        );

        Self { active, keys }
    }

    /// A store backed by a freshly generated key, for tests.
    #[cfg(test)]
    pub fn ephemeral() -> Self {
        Self::new(SecretKey::generate(), Vec::new())
    }

    /// The id of the key new values are sealed with.
    pub fn active_key_id(&self) -> KeyId {
        self.active.id()
    }

    /// Encrypts `plaintext`, binding it to `context`.
    pub fn seal(
        &self,
        plaintext: &[u8],
        context: SecretContext<'_>,
    ) -> Result<Sealed, human_errors::Error> {
        let aad = context.to_string();
        let nonce = Nonce::<Aes256Gcm>::generate();

        let ciphertext = self
            .active
            .cipher()
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| {
                human_errors::system(
                    "We could not encrypt a secret before storing it.",
                    &["This is unexpected; please report it with the surrounding log entries."],
                )
            })?;

        Ok(Sealed {
            v: ENVELOPE_VERSION,
            kid: self.active.id().to_string(),
            n: B64.encode(nonce),
            c: B64.encode(ciphertext),
        })
    }

    /// Decrypts a value, requiring that it was sealed against `context`.
    pub fn open(
        &self,
        sealed: &Sealed,
        context: SecretContext<'_>,
    ) -> Result<Vec<u8>, human_errors::Error> {
        if sealed.v != ENVELOPE_VERSION {
            return Err(human_errors::system(
                format!(
                    "A stored secret uses envelope version {}, which this version of Automate does not understand.",
                    sealed.v
                ),
                &["This usually means the agent was downgraded; run the newer version instead."],
            ));
        }

        let key = self.key_for(&sealed.kid)?;

        let nonce = B64.decode(&sealed.n).wrap_system_err(
            "A stored secret has a malformed nonce and cannot be decrypted.",
            &["The record may be corrupt; removing and recreating it will resolve this."],
        )?;
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce.as_slice()).map_err(|_| {
            human_errors::system(
                "A stored secret has a nonce of the wrong length and cannot be decrypted.",
                &["The record may be corrupt; removing and recreating it will resolve this."],
            )
        })?;

        let ciphertext = B64.decode(&sealed.c).wrap_system_err(
            "A stored secret has malformed ciphertext and cannot be decrypted.",
            &["The record may be corrupt; removing and recreating it will resolve this."],
        )?;

        let aad = context.to_string();

        key.cipher()
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| {
                // A GCM failure cannot distinguish a wrong key from a tampered
                // or relocated ciphertext, so the advice covers both.
                human_errors::system(
                    "A stored secret could not be decrypted.",
                    &[
                        "Check that your encryption key has not changed; a rotated key must be listed under 'previous_secret_keys'.",
                        "If the key is correct, the record may have been tampered with and should be recreated.",
                    ],
                )
            })
    }

    /// Encrypts a value's JSON representation.
    pub fn seal_json<T: Serialize>(
        &self,
        value: &T,
        context: SecretContext<'_>,
    ) -> Result<Sealed, human_errors::Error> {
        let plaintext = serde_json::to_vec(value).wrap_system_err(
            "We could not serialise a secret before encrypting it.",
            &["This is unexpected; please report it with the surrounding log entries."],
        )?;

        self.seal(&plaintext, context)
    }

    /// Decrypts a value and parses its JSON representation.
    pub fn open_json<T: DeserializeOwned>(
        &self,
        sealed: &Sealed,
        context: SecretContext<'_>,
    ) -> Result<T, human_errors::Error> {
        let plaintext = self.open(sealed, context)?;

        serde_json::from_slice(&plaintext).wrap_system_err(
            "A stored secret was decrypted but could not be parsed.",
            &["The record may have been written by a different version of Automate."],
        )
    }

    fn key_for(&self, kid: &str) -> Result<&SecretKey, human_errors::Error> {
        self.keys
            .iter()
            .find(|(id, _)| id.to_string() == kid)
            .map(|(_, key)| key)
            .ok_or_else(|| {
                human_errors::user(
                    format!("A stored secret was sealed with key '{kid}', which is not configured."),
                    &[
                        "Add the key that was previously in use to 'previous_secret_keys' under [web.auth].",
                        "If that key is lost, the affected connections and webhook secrets must be recreated.",
                    ],
                )
            })
    }
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SecretStore(active {}, {} key(s) available)",
            self.active.id(),
            self.keys.len()
        )
    }
}

/// Resolves the key file that accompanies a given database.
pub fn key_file_for(database: &Path) -> PathBuf {
    let mut path = database.as_os_str().to_owned();
    path.push(KEY_FILE_SUFFIX);

    PathBuf::from(path)
}

/// Loads the active key, generating and persisting one if none is configured.
///
/// Generating a key on first run rather than demanding configuration is what
/// lets an existing single-tenant install upgrade without the operator having
/// to do anything. The generated key lives beside the database rather than
/// inside it, so that a database backup is not by itself enough to read the
/// secrets it contains.
pub fn load_or_create_key(
    configured: Option<&str>,
    database: &Path,
) -> Result<SecretKey, human_errors::Error> {
    if let Some(configured) = configured.map(str::trim).filter(|k| !k.is_empty()) {
        return SecretKey::from_encoded(configured);
    }

    let path = key_file_for(database);

    if path.exists() {
        let contents = std::fs::read_to_string(&path).wrap_user_err(
            format!(
                "We could not read the encryption key at '{}'.",
                path.display()
            ),
            &[
                "Check that the file is readable by the user the agent runs as.",
                "Set 'secret_key' under [web.auth] to manage the key yourself instead.",
            ],
        )?;

        warn_if_world_readable(&path);

        return SecretKey::from_encoded(&contents);
    }

    let key = SecretKey::generate();
    write_key_file(&path, &key)?;

    warn!(
        key_file = %path.display(),
        key_id = %key.id(),
        "No encryption key was configured, so one has been generated. Back this file up: without it, stored credentials cannot be recovered."
    );

    Ok(key)
}

/// Writes a key file readable only by its owner.
fn write_key_file(path: &Path, key: &SecretKey) -> Result<(), human_errors::Error> {
    let contents = key.to_encoded();

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        // Created with restrictive permissions from the outset rather than
        // chmod-ed afterwards, which would leave a window where the key is
        // world-readable.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .wrap_user_err(
                format!("We could not create an encryption key at '{}'.", path.display()),
                &[
                    "Check that the directory exists and is writable by the user the agent runs as.",
                    "Set 'secret_key' under [web.auth] to manage the key yourself instead.",
                ],
            )?;

        file.write_all(contents.as_bytes()).wrap_user_err(
            format!(
                "We could not write the encryption key at '{}'.",
                path.display()
            ),
            &["Check that there is free space on the volume holding the key file."],
        )?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, contents.as_bytes()).wrap_user_err(
            format!(
                "We could not create an encryption key at '{}'.",
                path.display()
            ),
            &[
                "Check that the directory exists and is writable by the user the agent runs as.",
                "Set 'secret_key' under [web.auth] to manage the key yourself instead.",
            ],
        )?;
    }

    Ok(())
}

/// Warns when a key file is readable by users other than its owner.
fn warn_if_world_readable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Ok(metadata) = std::fs::metadata(path) {
            let mode = metadata.permissions().mode() & 0o077;

            if mode != 0 {
                warn!(
                    key_file = %path.display(),
                    "The encryption key file is readable by other users on this host. Restrict it with: chmod 600 {}",
                    path.display()
                );
            }
        }
    }

    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection_context(tenant: &str) -> SecretContext<'_> {
        SecretContext::Connection {
            tenant,
            connection: ConnectionId::from_entropy(1234),
        }
    }

    #[test]
    fn a_sealed_value_opens_again_under_the_same_context() {
        let store = SecretStore::ephemeral();
        let secret = b"todoist-api-token";

        let sealed = store.seal(secret, connection_context("alice")).unwrap();
        let opened = store.open(&sealed, connection_context("alice")).unwrap();

        assert_eq!(opened, secret);
    }

    #[test]
    fn the_ciphertext_does_not_contain_the_plaintext() {
        let store = SecretStore::ephemeral();
        let sealed = store
            .seal(b"todoist-api-token", connection_context("alice"))
            .unwrap();

        let rendered = serde_json::to_string(&sealed).unwrap();
        assert!(!rendered.contains("todoist"));
    }

    #[test]
    fn sealing_the_same_value_twice_produces_different_ciphertext() {
        // A fresh nonce per message means an observer cannot tell that two
        // users hold the same credential, or that a value was unchanged by an
        // update.
        let store = SecretStore::ephemeral();
        let context = connection_context("alice");

        let first = store.seal(b"same", context.clone()).unwrap();
        let second = store.seal(b"same", context).unwrap();

        assert_ne!(first.c, second.c);
        assert_ne!(first.n, second.n);
    }

    #[test]
    fn a_ciphertext_moved_to_another_tenant_will_not_open() {
        // The property the whole design exists for: an attacker with database
        // write access but no key cannot graft one tenant's credential onto
        // another tenant's record.
        let store = SecretStore::ephemeral();
        let sealed = store
            .seal(b"alices-token", connection_context("alice"))
            .unwrap();

        let err = store
            .open(&sealed, connection_context("mallory"))
            .unwrap_err();
        assert!(err.to_string().contains("could not be decrypted"));
    }

    #[test]
    fn a_ciphertext_moved_to_another_kind_of_record_will_not_open() {
        let store = SecretStore::ephemeral();
        let sealed = store
            .seal(b"alices-token", connection_context("alice"))
            .unwrap();

        let relocated = SecretContext::WebhookSecret {
            tenant: "alice",
            workflow: WorkflowId::from_entropy(1234),
        };

        assert!(store.open(&sealed, relocated).is_err());
    }

    #[test]
    fn tampering_with_the_ciphertext_is_detected() {
        let store = SecretStore::ephemeral();
        let mut sealed = store.seal(b"token", connection_context("alice")).unwrap();

        let mut bytes = B64.decode(&sealed.c).unwrap();
        bytes[0] ^= 0x01;
        sealed.c = B64.encode(&bytes);

        assert!(store.open(&sealed, connection_context("alice")).is_err());
    }

    #[test]
    fn a_value_sealed_with_a_retired_key_still_opens() {
        let retired = SecretKey::generate();
        let retired_id = retired.id();

        let old_store = SecretStore::new(
            SecretKey::from_encoded(&retired.to_encoded()).unwrap(),
            vec![],
        );
        let sealed = old_store
            .seal(b"token", connection_context("alice"))
            .unwrap();
        assert_eq!(sealed.kid, retired_id.to_string());

        let rotated = SecretStore::new(SecretKey::generate(), vec![retired]);
        assert_eq!(
            rotated.open(&sealed, connection_context("alice")).unwrap(),
            b"token"
        );

        // New values use the new key, so the store drains off the old one as
        // records are rewritten.
        let resealed = rotated.seal(b"token", connection_context("alice")).unwrap();
        assert_eq!(resealed.kid, rotated.active_key_id().to_string());
    }

    #[test]
    fn a_value_sealed_with_an_unknown_key_explains_what_to_do() {
        let store = SecretStore::ephemeral();
        let orphan = SecretStore::ephemeral()
            .seal(b"token", connection_context("alice"))
            .unwrap();

        let err = store
            .open(&orphan, connection_context("alice"))
            .unwrap_err();
        assert!(err.to_string().contains("previous_secret_keys"), "{err}");
    }

    #[test]
    fn json_values_round_trip_through_the_envelope() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Credential {
            access_token: String,
            refresh_token: String,
        }

        let store = SecretStore::ephemeral();
        let credential = Credential {
            access_token: "at".into(),
            refresh_token: "rt".into(),
        };

        let sealed = store
            .seal_json(&credential, connection_context("alice"))
            .unwrap();
        let opened: Credential = store
            .open_json(&sealed, connection_context("alice"))
            .unwrap();

        assert_eq!(opened, credential);
    }

    #[test]
    fn envelopes_survive_a_round_trip_through_storage() {
        let store = SecretStore::ephemeral();
        let sealed = store.seal(b"token", connection_context("alice")).unwrap();

        // Envelopes are stored as ordinary JSON values in the key-value store,
        // so they must survive that encoding unchanged.
        let stored = serde_json::to_value(&sealed).unwrap();
        let loaded: Sealed = serde_json::from_value(stored).unwrap();

        assert_eq!(loaded, sealed);
        assert_eq!(
            store.open(&loaded, connection_context("alice")).unwrap(),
            b"token"
        );
    }

    #[test]
    fn keys_are_accepted_in_the_encodings_operators_actually_paste() {
        let key = SecretKey::generate();
        let raw = B64.decode(key.to_encoded()).unwrap();

        let url_safe = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw);
        let standard = base64::engine::general_purpose::STANDARD.encode(&raw);
        let hexadecimal = hex::encode(&raw);

        for encoded in [url_safe, standard, hexadecimal] {
            let parsed = SecretKey::from_encoded(&encoded).unwrap();
            assert_eq!(parsed.id(), key.id(), "failed for {encoded}");
        }
    }

    #[test]
    fn a_key_of_the_wrong_length_is_rejected_with_its_actual_length() {
        let short = B64.encode([0u8; 16]);
        let err = SecretKey::from_encoded(&short).unwrap_err();

        assert!(err.to_string().contains("16 bytes"), "{err}");
    }

    #[test]
    fn an_unresolved_environment_expression_is_diagnosed_specifically() {
        // The config loader leaves these in place when the variable is unset,
        // so this is the most common way a key ends up malformed.
        let err = SecretKey::from_encoded("${{ env.AUTOMATE_SECRET_KEY }}").unwrap_err();

        assert!(err.to_string().contains("unresolved"), "{err}");
    }

    #[test]
    fn an_empty_key_is_diagnosed_specifically() {
        let err = SecretKey::from_encoded("   ").unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn key_ids_identify_keys_without_revealing_them() {
        let key = SecretKey::generate();
        let id = key.id().to_string();

        assert_eq!(id.len(), 8);
        assert!(!key.to_encoded().contains(&id));

        // Stable across parses of the same material.
        assert_eq!(
            SecretKey::from_encoded(&key.to_encoded()).unwrap().id(),
            key.id()
        );

        assert_ne!(SecretKey::generate().id(), SecretKey::generate().id());
    }

    #[test]
    fn debug_output_never_reveals_key_material_or_ciphertext() {
        let key = SecretKey::generate();
        assert!(!format!("{key:?}").contains(&key.to_encoded()));

        let store = SecretStore::ephemeral();
        let sealed = store.seal(b"token", connection_context("alice")).unwrap();
        assert!(!format!("{sealed:?}").contains(&sealed.c));
        assert!(!format!("{store:?}").contains(&store.active.to_encoded()));
    }

    #[test]
    fn a_configured_key_is_preferred_over_generating_one() {
        let dir = std::env::temp_dir().join(format!("automate-crypto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("database.sqlite");

        let configured = SecretKey::generate();
        let loaded = load_or_create_key(Some(&configured.to_encoded()), &database).unwrap();

        assert_eq!(loaded.id(), configured.id());
        assert!(
            !key_file_for(&database).exists(),
            "a key file should not be created when one is configured"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_generated_key_is_persisted_and_reused_on_the_next_start() {
        let dir = std::env::temp_dir().join(format!("automate-crypto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("database.sqlite");

        let first = load_or_create_key(None, &database).unwrap();
        let key_file = key_file_for(&database);
        assert!(key_file.exists());

        // Restarting must not invalidate everything sealed by the last run.
        let second = load_or_create_key(None, &database).unwrap();
        assert_eq!(first.id(), second.id());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key_file).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the key file must not be readable by other users"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_configured_key_falls_back_to_the_key_file() {
        // Distinguishes "explicitly blank" from "absent"; an operator whose
        // environment variable expands to nothing should not be left without a
        // working install.
        let dir = std::env::temp_dir().join(format!("automate-crypto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("database.sqlite");

        let key = load_or_create_key(Some("  "), &database).unwrap();
        assert_eq!(load_or_create_key(None, &database).unwrap().id(), key.id());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_key_file_sits_beside_the_database() {
        assert_eq!(
            key_file_for(Path::new("/var/lib/automate/database.sqlite")),
            PathBuf::from("/var/lib/automate/database.sqlite.key")
        );
    }
}
