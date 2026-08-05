//! Tenant identity.
//!
//! Automate namespaces every stored record by the user who owns it. That
//! namespace key is the user's OIDC username, used directly rather than mapped
//! through an internal surrogate, so a tenant is legible wherever it appears —
//! in the database, in a log line, in the `X-Impersonate-User` header an
//! administrator sends.
//!
//! The trade-off is that renaming a user in the identity provider orphans their
//! records, since the name *is* the key. That is recoverable — an administrator
//! can rename a tenant, which rewrites the key across every table — but it is a
//! deliberate operation rather than something that happens by itself.
//!
//! # Reserved tenants
//!
//! Two tenants are owned by the installation rather than by a person, and are
//! distinguished by a leading `!`, which no identity provider will emit in a
//! username:
//!
//! - [`TenantId::SYSTEM`] holds installation-wide state that belongs to nobody:
//!   the user registry, discovery caches, and the indexes that map an inbound
//!   webhook to the tenant that should receive it.
//! - [`TenantId::LOCAL`] owns everything in an installation that has no identity
//!   provider configured. A single-tenant install therefore keeps working
//!   untouched, and adopting OIDC later is a matter of renaming this tenant to
//!   the username you sign in as.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// The prefix marking a tenant as owned by the installation rather than a user.
const RESERVED_PREFIX: char = '!';

/// The longest tenant name we will accept.
///
/// Comfortably beyond any real username, while leaving room for the tenant to
/// sit at the front of a composite database key without bloating the index.
const MAX_LENGTH: usize = 190;

/// The namespace that a stored record belongs to.
///
/// Tenant names are compared and stored in lower case. Identity providers are
/// inconsistent about the casing of `preferred_username` — Active Directory
/// backed ones especially — and a user whose records silently split across
/// `Alice` and `alice` is a far worse outcome than the theoretical case of two
/// distinct users differing only by case.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TenantId(String);

impl TenantId {
    /// The tenant holding installation-wide state that belongs to no user.
    pub const SYSTEM: &'static str = "!system";

    /// The tenant owning everything in an install with no identity provider.
    pub const LOCAL: &'static str = "!local";

    /// The [`TenantId::SYSTEM`] tenant.
    pub fn system() -> Self {
        Self(Self::SYSTEM.to_string())
    }

    /// The [`TenantId::LOCAL`] tenant.
    pub fn local() -> Self {
        Self(Self::LOCAL.to_string())
    }

    /// Builds a tenant from a username supplied by an identity provider.
    ///
    /// Rejects names that would collide with a reserved tenant or that cannot
    /// be used unambiguously as a key.
    pub fn new(username: impl AsRef<str>) -> Result<Self, TenantIdError> {
        let username = username.as_ref().trim();

        if username.is_empty() {
            return Err(TenantIdError::Empty);
        }

        if username.len() > MAX_LENGTH {
            return Err(TenantIdError::TooLong {
                length: username.len(),
                max: MAX_LENGTH,
            });
        }

        if username.starts_with(RESERVED_PREFIX) {
            return Err(TenantIdError::Reserved {
                username: username.to_string(),
            });
        }

        // `/` separates the fields of the context strings that bind an
        // encrypted value to its row, so allowing it in a tenant name would
        // make two different rows able to produce the same context.
        if let Some(character) = username
            .chars()
            .find(|c| *c == '/' || c.is_control() || c.is_whitespace())
        {
            return Err(TenantIdError::IllegalCharacter {
                username: username.to_string(),
                character,
            });
        }

        Ok(Self(username.to_lowercase()))
    }

    /// Wraps a value already known to be a valid tenant, such as one read back
    /// out of the database.
    ///
    /// Applies the same case normalisation as [`TenantId::new`] but performs no
    /// validation, so that a row written by an older version with rules we have
    /// since tightened stays readable rather than becoming unloadable.
    pub fn from_storage(value: impl AsRef<str>) -> Self {
        Self(value.as_ref().to_lowercase())
    }

    /// The tenant name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this tenant belongs to the installation rather than a user.
    pub fn is_reserved(&self) -> bool {
        self.0.starts_with(RESERVED_PREFIX)
    }

    /// Whether this is the [`TenantId::SYSTEM`] tenant.
    pub fn is_system(&self) -> bool {
        self.0 == Self::SYSTEM
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TenantId({})", self.0)
    }
}

impl AsRef<str> for TenantId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for TenantId {
    type Err = TenantIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl Serialize for TenantId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TenantId {
    /// Deserialises without validation, using [`TenantId::from_storage`].
    ///
    /// Values reaching this path have already been through [`TenantId::new`] on
    /// the way in; re-validating here would make a stored record unreadable if
    /// the rules were ever tightened.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TenantVisitor;

        impl de::Visitor<'_> for TenantVisitor {
            type Value = TenantId;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a tenant name")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(TenantId::from_storage(value))
            }
        }

        deserializer.deserialize_str(TenantVisitor)
    }
}

/// The ways a username can fail to be usable as a tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantIdError {
    /// The username was blank.
    Empty,

    /// The username exceeded [`MAX_LENGTH`].
    TooLong { length: usize, max: usize },

    /// The username collided with the reserved namespace.
    Reserved { username: String },

    /// The username contained a character that cannot appear in a tenant name.
    IllegalCharacter { username: String, character: char },
}

impl fmt::Display for TenantIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str(
                "Your identity provider did not supply a username, so we cannot tell which account this is.",
            ),
            Self::TooLong { length, max } => write!(
                f,
                "The username supplied by your identity provider is {length} characters long, but the maximum supported is {max}."
            ),
            Self::Reserved { username } => write!(
                f,
                "The username '{username}' is reserved for internal use by Automate and cannot be used to sign in."
            ),
            Self::IllegalCharacter { username, character } => write!(
                f,
                "The username '{username}' contains the character '{character}', which cannot be used in an account name."
            ),
        }
    }
}

impl std::error::Error for TenantIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_usernames_are_accepted() {
        for username in [
            "alice",
            "alice.smith",
            "alice@example.com",
            "alice-smith",
            "alice_smith",
            "CORP\\alice",
            "0d4f1a6e-1d2b-4c3a-9f8e-7a6b5c4d3e2f",
        ] {
            assert!(
                TenantId::new(username).is_ok(),
                "{username} should be a usable tenant"
            );
        }
    }

    #[test]
    fn usernames_are_normalised_to_lower_case() {
        // Identity providers vary the casing they return, and a user whose
        // records split across two spellings is worse than the theoretical
        // collision this risks.
        assert_eq!(
            TenantId::new("Alice").unwrap(),
            TenantId::new("alice").unwrap()
        );
        assert_eq!(
            TenantId::new("ALICE@Example.COM").unwrap().as_str(),
            "alice@example.com"
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_rather_than_rejected() {
        assert_eq!(TenantId::new("  alice  ").unwrap().as_str(), "alice");
    }

    #[test]
    fn the_reserved_namespace_cannot_be_claimed_by_a_user() {
        // Otherwise a user called '!system' would be handed the user registry.
        for username in ["!system", "!local", "!anything"] {
            assert!(matches!(
                TenantId::new(username),
                Err(TenantIdError::Reserved { .. })
            ));
        }
    }

    #[test]
    fn characters_that_would_make_a_key_ambiguous_are_rejected() {
        // '/' separates fields in the context that binds a ciphertext to its
        // row, so permitting it would let two different rows share a context.
        assert!(matches!(
            TenantId::new("alice/bob"),
            Err(TenantIdError::IllegalCharacter { character: '/', .. })
        ));

        assert!(matches!(
            TenantId::new("alice bob"),
            Err(TenantIdError::IllegalCharacter { .. })
        ));

        assert!(matches!(
            TenantId::new("alice\nbob"),
            Err(TenantIdError::IllegalCharacter { .. })
        ));
    }

    #[test]
    fn empty_and_overlong_usernames_are_rejected() {
        assert_eq!(TenantId::new(""), Err(TenantIdError::Empty));
        assert_eq!(TenantId::new("   "), Err(TenantIdError::Empty));

        assert!(matches!(
            TenantId::new("a".repeat(MAX_LENGTH + 1)),
            Err(TenantIdError::TooLong { .. })
        ));
        assert!(TenantId::new("a".repeat(MAX_LENGTH)).is_ok());
    }

    #[test]
    fn the_reserved_tenants_are_recognisable() {
        assert!(TenantId::system().is_reserved());
        assert!(TenantId::system().is_system());

        assert!(TenantId::local().is_reserved());
        assert!(!TenantId::local().is_system());

        assert!(!TenantId::new("alice").unwrap().is_reserved());
    }

    #[test]
    fn tenants_round_trip_through_serde() {
        let tenant = TenantId::new("alice@example.com").unwrap();
        let json = serde_json::to_string(&tenant).unwrap();

        assert_eq!(json, "\"alice@example.com\"");
        assert_eq!(serde_json::from_str::<TenantId>(&json).unwrap(), tenant);
    }

    #[test]
    fn stored_values_load_even_if_they_would_fail_todays_validation() {
        // A record written by an older version must stay readable if the rules
        // are tightened later, so loading does not re-validate.
        let stored = TenantId::from_storage("alice/legacy");

        assert_eq!(stored.as_str(), "alice/legacy");
        assert_eq!(
            serde_json::from_str::<TenantId>("\"alice/legacy\"").unwrap(),
            stored
        );
    }

    #[test]
    fn reserved_tenants_round_trip_through_storage() {
        for reserved in [TenantId::system(), TenantId::local()] {
            let json = serde_json::to_string(&reserved).unwrap();
            assert_eq!(serde_json::from_str::<TenantId>(&json).unwrap(), reserved);
        }
    }
}
