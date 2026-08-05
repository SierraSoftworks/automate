//! The secret half of a webhook ingress URL.
//!
//! [`ids`](crate::ids) notes that a word identifier is a *name*, never a
//! credential: three words are memorable precisely because they are short,
//! which also makes them enumerable. A webhook ingress URL is reachable by
//! anyone on the internet and is authenticated by nothing but the URL itself,
//! so it carries a [`WebhookToken`] alongside the workflow's identifier:
//!
//! ```text
//! /api/v1/hooks/copper-tiger-canyon/nT8xR2vQm4KbZ7pL1sYd9A
//!                └─ the name ─┘     └────── the secret ──────┘
//! ```
//!
//! The name says which workflow to run; the token is the sole proof that the
//! caller is allowed to run it.
//!
//! # Sizing
//!
//! A token is 128 bits. Unlike an identifier there is no memorability budget to
//! respect — nobody types a webhook URL from memory, it is copied out of the UI
//! and pasted into the calling system — so the size is chosen purely so that
//! guessing is hopeless. At 2^128 possibilities an attacker enumerating the
//! space at a billion attempts a second is still working on it long after the
//! heat death of anything that cares.
//!
//! # Where tokens come from
//!
//! This crate compiles to WebAssembly for the UI as well as natively for the
//! agent, and it deliberately carries only `chrono`, `serde` and `serde_json`.
//! Pulling in a randomness source would drag a platform-specific entropy
//! backend into a WASM build for the sake of an operation the UI must never
//! perform anyway — a browser has no business minting a credential.
//!
//! So there is no `generate()` here. [`WebhookToken::from_bytes`] takes the
//! sixteen bytes, and the agent — the only component that should ever mint one
//! — supplies them from a cryptographically secure source. This type owns the
//! *representation* of a token; the agent owns its *creation*.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// The number of bytes of entropy in a token.
pub const TOKEN_BYTES: usize = 16;

/// The number of characters in a token's wire form.
///
/// Base64 packs 6 bits per character, so 128 bits need `ceil(128 / 6) = 22`
/// characters. Those 22 characters span 132 bits, and the 4 bits of slack live
/// in the low end of the final character.
pub const ENCODED_LEN: usize = 22;

/// The URL-safe base64 alphabet, as defined by RFC 4648 §5.
///
/// This differs from the standard alphabet only in its final two entries: `-`
/// and `_` in place of `+` and `/`, neither of which survives a trip through a
/// URL path segment unescaped.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// The unguessable half of a webhook ingress URL.
///
/// 128 bits, rendered on the wire as 22 characters of unpadded URL-safe base64.
/// Padding is omitted because the length is fixed and known, so a `=` would
/// carry no information while needing to be percent-encoded in a URL.
///
/// This is a credential. It is compared in constant time, and its [`Debug`]
/// output is redacted.
///
/// [`Debug`]: fmt::Debug
#[derive(Clone, Copy)]
pub struct WebhookToken([u8; TOKEN_BYTES]);

impl WebhookToken {
    /// Wraps sixteen bytes of caller-supplied randomness.
    ///
    /// The bytes **must** come from a cryptographically secure generator. The
    /// whole security of a webhook endpoint rests on them being unpredictable:
    /// there is no second factor behind the URL, and an ingress endpoint is
    /// reachable by anyone who can reach the installation.
    ///
    /// Randomness is not sourced here on purpose — see the [module
    /// documentation](self) for why this crate must not depend on an entropy
    /// backend.
    pub const fn from_bytes(bytes: [u8; TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    /// The raw bytes, for storage or for deriving a lookup key.
    ///
    /// Prefer [`Display`](fmt::Display) when producing a value a human or
    /// another system will see; this is the form to persist or hash.
    pub const fn as_bytes(&self) -> &[u8; TOKEN_BYTES] {
        &self.0
    }

    /// Renders the token into its 22 ASCII characters.
    ///
    /// Bytes are consumed three at a time, each triple packed into the top of a
    /// 24-bit accumulator and emitted as four 6-bit characters. Packing from
    /// the top means the final, short group leaves its slack in the low bits
    /// rather than shifting the meaningful ones, so no special case is needed
    /// for the trailing byte.
    fn encode(&self) -> [u8; ENCODED_LEN] {
        let mut encoded = [0u8; ENCODED_LEN];
        let mut cursor = 0;

        for group in self.0.chunks(3) {
            let mut block = 0u32;
            for (index, byte) in group.iter().enumerate() {
                block |= (*byte as u32) << (16 - 8 * index);
            }

            // Each byte contributes 8 bits and each character carries 6, so a
            // group of `n` bytes rounds up to `ceil(n * 8 / 6)` characters.
            for index in 0..(group.len() * 8).div_ceil(6) {
                encoded[cursor] = ALPHABET[((block >> (18 - 6 * index)) & 0x3F) as usize];
                cursor += 1;
            }
        }

        encoded
    }
}

/// Maps a character to its 6-bit value, or `None` if it is outside the
/// URL-safe alphabet.
///
/// Written as a match over ranges rather than a search of [`ALPHABET`] so that
/// the mapping is obvious by inspection, which matters more here than it would
/// for a general-purpose codec.
fn sextet(character: char) -> Option<u8> {
    match character {
        'A'..='Z' => Some(character as u8 - b'A'),
        'a'..='z' => Some(character as u8 - b'a' + 26),
        '0'..='9' => Some(character as u8 - b'0' + 52),
        '-' => Some(62),
        '_' => Some(63),
        _ => None,
    }
}

impl fmt::Display for WebhookToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let encoded = self.encode();

        // Every byte is drawn from `ALPHABET`, which is entirely ASCII.
        let encoded = core::str::from_utf8(&encoded).expect("the base64 alphabet is ASCII");

        f.write_str(encoded)
    }
}

impl fmt::Debug for WebhookToken {
    /// Redacts the token rather than rendering it.
    ///
    /// A token is a bearer credential, and `{:?}` is how values end up in log
    /// lines, panic messages and error reports — usually indirectly, because
    /// the token was a field of some larger struct that derived `Debug`. A
    /// credential written to a log has to be treated as disclosed, so the safe
    /// default belongs here rather than at every call site that might print
    /// something containing one.
    ///
    /// The bytes are still reachable through [`WebhookToken::as_bytes`] or
    /// [`Display`](fmt::Display), both of which are explicit acts.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebhookToken(********)")
    }
}

impl FromStr for WebhookToken {
    type Err = WebhookTokenError;

    /// Parses the wire form, which must be exactly [`ENCODED_LEN`] characters
    /// of unpadded URL-safe base64.
    ///
    /// Nothing is tolerated here — no case folding, no stray separators, no
    /// `=` padding, no whitespace trimming. A [`WordId`](crate::WordId) is
    /// forgiving because a person retypes one from a screen; a token is only
    /// ever copied and pasted by machines, so anything that does not match
    /// exactly is a bug or an attack rather than a typo.
    ///
    /// The 4 low bits of the final character fall outside the 128 bits of the
    /// token and are discarded, so a handful of encodings map onto the same
    /// value. That costs nothing: producing any of them requires already
    /// holding the token.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let found = s.chars().count();
        if found != ENCODED_LEN {
            return Err(WebhookTokenError::WrongLength {
                found,
                expected: ENCODED_LEN,
            });
        }

        let mut sextets = [0u8; ENCODED_LEN];
        for (slot, character) in sextets.iter_mut().zip(s.chars()) {
            *slot = sextet(character).ok_or(WebhookTokenError::InvalidCharacter { character })?;
        }

        // Twenty-two characters carry 132 bits and a token is 128, so the last
        // character has four bits left over. Anything other than zero there is
        // a second spelling of a token that already has one, and a credential
        // with sixteen spellings is a credential that can be revoked in one
        // and still presented in another.
        if sextets[ENCODED_LEN - 1] & 0x0F != 0 {
            return Err(WebhookTokenError::NotCanonical);
        }

        // The inverse of `encode`: four characters refill a 24-bit accumulator
        // from the top, yielding three bytes; the trailing pair of characters
        // yields the one remaining byte.
        let mut bytes = [0u8; TOKEN_BYTES];
        let mut cursor = 0;

        for group in sextets.chunks(4) {
            let mut block = 0u32;
            for (index, value) in group.iter().enumerate() {
                block |= (*value as u32) << (18 - 6 * index);
            }

            for index in 0..(group.len() * 6 / 8) {
                bytes[cursor] = ((block >> (16 - 8 * index)) & 0xFF) as u8;
                cursor += 1;
            }
        }

        Ok(Self(bytes))
    }
}

impl PartialEq for WebhookToken {
    /// Compares two tokens without short-circuiting.
    ///
    /// One side of this comparison is attacker-supplied: it is whatever
    /// arrived in the webhook URL. A byte-at-a-time comparison that returned
    /// early on the first mismatch would take measurably longer the more of
    /// the prefix the attacker guessed correctly, which turns a 2^128 search
    /// into 16 sequential 2^8 searches — trivially feasible.
    ///
    /// So every byte is always examined, folding differences into an
    /// accumulator that is only inspected at the end. This is best-effort
    /// rather than a guarantee: without an optimisation barrier, a sufficiently
    /// determined compiler is entitled to reintroduce a branch. The fold has no
    /// data-dependent control flow to work with, which in practice is enough.
    fn eq(&self, other: &Self) -> bool {
        let difference = self
            .0
            .iter()
            .zip(other.0)
            .fold(0u8, |accumulator, (left, right)| {
                accumulator | (left ^ right)
            });

        difference == 0
    }
}

impl Eq for WebhookToken {}

impl Hash for WebhookToken {
    /// Hashes the raw bytes.
    ///
    /// Written by hand rather than derived because deriving `Hash` alongside a
    /// manual `PartialEq` is a well-known footgun — the two can disagree. They
    /// do not here: [`PartialEq`] is bytewise equality computed in constant
    /// time, so equal tokens hash equally.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Serialize for WebhookToken {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WebhookToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WebhookTokenVisitor;

        impl de::Visitor<'_> for WebhookTokenVisitor {
            type Value = WebhookToken;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    "a webhook token of {ENCODED_LEN} URL-safe base64 characters"
                )
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse().map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_str(WebhookTokenVisitor)
    }
}

/// The ways a string can fail to be a [`WebhookToken`].
///
/// Deliberately says nothing about *which* token was expected or how close the
/// input came to it, since these messages travel back to whoever called the
/// ingress endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookTokenError {
    /// The token decodes, but is not the spelling this token would be written
    /// as. Refused so that one credential has exactly one written form.
    NotCanonical,

    /// The input was not [`ENCODED_LEN`] characters long.
    WrongLength { found: usize, expected: usize },

    /// The input contained a character outside the URL-safe base64 alphabet.
    InvalidCharacter { character: char },
}

impl fmt::Display for WebhookTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { found, expected } => write!(
                f,
                "A webhook token should be {expected} characters long, but {found} were provided."
            ),
            Self::NotCanonical => write!(
                f,
                "This is not how a webhook token is written, even though it decodes to one. Use the token exactly as it was given to you."
            ),
            Self::InvalidCharacter { character } => write!(
                f,
                "A webhook token contains only letters, digits, '-' and '_', but this one contains '{character}'."
            ),
        }
    }
}

impl std::error::Error for WebhookTokenError {}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::hash_map::DefaultHasher;
    use std::collections::{HashMap, HashSet};

    /// Produces deterministic pseudo-random tokens.
    ///
    /// A xorshift written out here rather than pulled in as a dependency, for
    /// the same reason the type has no `generate()`: this crate stays bare.
    /// Determinism also means a failure reproduces exactly.
    fn sample_tokens(count: usize) -> Vec<WebhookToken> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;

        (0..count)
            .map(|_| {
                let mut bytes = [0u8; TOKEN_BYTES];
                for byte in bytes.iter_mut() {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    *byte = (state >> 24) as u8;
                }

                WebhookToken::from_bytes(bytes)
            })
            .collect()
    }

    #[test]
    fn the_encoded_length_follows_from_the_token_size() {
        // 128 bits at 6 bits per character. If either constant is ever changed
        // without the other, every fixed-width assumption below breaks.
        assert_eq!(ENCODED_LEN, (TOKEN_BYTES * 8).div_ceil(6));
        assert_eq!(TOKEN_BYTES, 16);
        assert_eq!(ENCODED_LEN, 22);
    }

    #[test]
    fn the_wire_form_is_twenty_two_characters_with_no_padding() {
        // The length is fixed and known to both ends, so padding would only
        // add a '=' that has to be percent-encoded in a URL path segment.
        for token in sample_tokens(256) {
            let encoded = token.to_string();

            assert_eq!(encoded.len(), ENCODED_LEN, "{encoded} is the wrong length");
            assert!(!encoded.contains('='), "{encoded} carries padding");
            assert!(
                encoded
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "{encoded} left the URL-safe alphabet"
            );
        }
    }

    #[test]
    fn the_alphabet_avoids_the_characters_a_url_would_mangle() {
        // '+' and '/' are the standard-base64 entries this replaces; either
        // one would need escaping inside a URL path segment.
        assert!(!ALPHABET.contains(&b'+'));
        assert!(!ALPHABET.contains(&b'/'));
        assert_eq!(ALPHABET[62], b'-');
        assert_eq!(ALPHABET[63], b'_');

        // Every entry must be distinct, or the encoding would not be
        // invertible.
        assert_eq!(ALPHABET.iter().collect::<HashSet<_>>().len(), 64);
    }

    #[test]
    fn the_extremes_of_the_space_encode_to_known_values() {
        // Fixed vectors, so a refactor of the bit-twiddling that happens to be
        // self-consistent still cannot silently change the wire format.
        assert_eq!(
            WebhookToken::from_bytes([0x00; TOKEN_BYTES]).to_string(),
            "AAAAAAAAAAAAAAAAAAAAAA"
        );
        assert_eq!(
            WebhookToken::from_bytes([0xFF; TOKEN_BYTES]).to_string(),
            "_____________________w"
        );
    }

    #[test]
    fn tokens_round_trip_through_their_string_form() {
        // The wire form is the only way a token travels, so an encoding that
        // did not invert exactly would lock a caller out of their own webhook.
        for token in sample_tokens(1024) {
            let encoded = token.to_string();
            let parsed: WebhookToken = encoded.parse().unwrap();

            assert_eq!(parsed, token, "{encoded} did not round-trip");
            assert_eq!(parsed.as_bytes(), token.as_bytes());
            assert_eq!(parsed.to_string(), encoded);
        }
    }

    #[test]
    fn every_bit_position_survives_the_round_trip() {
        // Walks a single set bit through all 128 positions. A shift that is off
        // by one shows up as a bit landing in the wrong byte, which a random
        // sweep can mask but this cannot.
        for bit in 0..TOKEN_BYTES * 8 {
            let mut bytes = [0u8; TOKEN_BYTES];
            bytes[bit / 8] = 1 << (bit % 8);

            let token = WebhookToken::from_bytes(bytes);
            let parsed: WebhookToken = token.to_string().parse().unwrap();

            assert_eq!(parsed.as_bytes(), &bytes, "bit {bit} was not preserved");
        }
    }

    #[test]
    fn distinct_tokens_encode_distinctly() {
        // Two tokens sharing a wire form would let one caller trigger the
        // other's workflow.
        let tokens = sample_tokens(1024);
        let encoded: HashSet<String> = tokens.iter().map(WebhookToken::to_string).collect();

        assert_eq!(encoded.len(), tokens.len());
    }

    #[test]
    fn tokens_round_trip_through_serde() {
        // Tokens are persisted and served as JSON strings, not as byte arrays,
        // so the serialised form must match the wire form exactly.
        for token in sample_tokens(64) {
            let json = serde_json::to_string(&token).unwrap();

            assert_eq!(json, format!("\"{token}\""));
            assert_eq!(serde_json::from_str::<WebhookToken>(&json).unwrap(), token);
        }
    }

    #[test]
    fn deserialising_a_malformed_token_explains_the_problem() {
        // The parse error has to survive the trip through serde, otherwise a
        // corrupted stored token surfaces as an opaque failure.
        let err = serde_json::from_str::<WebhookToken>("\"too-short\"").unwrap_err();
        assert!(
            err.to_string().contains("22 characters"),
            "unhelpful error: {err}"
        );

        let err = serde_json::from_str::<WebhookToken>("\"AAAAAAAAAA+AAAAAAAAAAA\"").unwrap_err();
        assert!(err.to_string().contains('+'), "unhelpful error: {err}");
    }

    #[test]
    fn input_of_the_wrong_length_is_rejected() {
        // A token is fixed width, so a short or long input cannot be a token
        // that merely needs trimming — accepting one would mean guessing.
        for input in ["", "A", "AAAAAAAAAAAAAAAAAAAAA", "AAAAAAAAAAAAAAAAAAAAAAA"] {
            let err = input.parse::<WebhookToken>().unwrap_err();

            assert_eq!(
                err,
                WebhookTokenError::WrongLength {
                    found: input.chars().count(),
                    expected: ENCODED_LEN,
                },
                "failed on {input:?}"
            );
            assert!(err.to_string().contains("22 characters"));
        }
    }

    #[test]
    fn the_padded_standard_base64_form_is_rejected() {
        // Standard base64 would render 16 bytes as 24 characters ending in
        // '=='. Silently accepting it would mean two wire formats for one
        // token, and the length check already catches it.
        let padded = format!("{}==", WebhookToken::from_bytes([0x00; TOKEN_BYTES]));

        assert!(matches!(
            padded.parse::<WebhookToken>(),
            Err(WebhookTokenError::WrongLength { found: 24, .. })
        ));
    }

    #[test]
    fn a_second_spelling_of_a_token_is_refused() {
        // Twenty-two characters carry four more bits than a token uses, so
        // sixteen strings would otherwise decode to the same credential. One of
        // them is the one we issued; the rest are ways to present a token that
        // has been revoked under its real spelling.
        let token = WebhookToken::from_bytes([0xFF; TOKEN_BYTES]);
        let issued = token.to_string();

        assert_eq!(issued.parse::<WebhookToken>().unwrap(), token);

        let characters: Vec<char> = issued.chars().collect();
        let last = characters.len() - 1;
        let issued_final = characters[last];
        let mut refused = 0;

        for candidate in ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'v', 'w', 'x', 'y', 'z'] {
            if candidate == issued_final {
                continue;
            }

            let mut characters = characters.clone();

            characters[last] = candidate;
            let spelling: String = characters.iter().collect();

            if let Ok(parsed) = spelling.parse::<WebhookToken>() {
                assert_ne!(
                    parsed, token,
                    "'{spelling}' was accepted as a second spelling of the same token",
                );
            } else {
                refused += 1;
            }
        }

        assert!(refused > 0, "no alternative spelling was refused at all");
    }

    #[test]
    fn input_with_characters_outside_the_alphabet_is_rejected() {
        // '+' and '/' are the standard-base64 characters someone might
        // reasonably send by mistake; the rest are the shapes an injection
        // attempt takes.
        for (input, expected) in [
            ("AAAAAAAAAAAAAAAAAAAAA+", '+'),
            ("AAAAAAAAAAAAAAAAAAAAA/", '/'),
            ("AAAAAAAAAA.AAAAAAAAAAA", '.'),
            ("AAAAAAAAAA AAAAAAAAAAA", ' '),
            ("AAAAAAAAAA\nAAAAAAAAAAA", '\n'),
            ("AAAAAAAAAA\0AAAAAAAAAAA", '\0'),
            ("AAAAAAAAAA%AAAAAAAAAAA", '%'),
            ("AAAAAAAAAAéAAAAAAAAAAA", 'é'),
        ] {
            assert_eq!(
                input.parse::<WebhookToken>().unwrap_err(),
                WebhookTokenError::InvalidCharacter {
                    character: expected
                },
                "failed on {input:?}"
            );
        }
    }

    #[test]
    fn parsing_does_not_fold_case_or_trim_whitespace() {
        // A `WordId` is forgiving because people retype one; a token is only
        // ever machine-copied, so leniency here would just widen the set of
        // strings that open the door.
        let token = WebhookToken::from_bytes([0x00; TOKEN_BYTES]);
        let encoded = token.to_string();

        // Either it is refused outright or it decodes to something else; what
        // matters is that a differently-cased copy is not the same credential.
        if let Ok(parsed) = encoded.to_lowercase().parse::<WebhookToken>() {
            assert_ne!(parsed, token);
        }

        assert!(format!(" {encoded} ").parse::<WebhookToken>().is_err());
    }

    #[test]
    fn equal_tokens_compare_equal_and_different_ones_do_not() {
        // The whole point of the constant-time fold is that it must still be a
        // correct equality test, not merely a slow one.
        let bytes = [7u8; TOKEN_BYTES];

        assert_eq!(
            WebhookToken::from_bytes(bytes),
            WebhookToken::from_bytes(bytes)
        );
        assert_ne!(
            WebhookToken::from_bytes(bytes),
            WebhookToken::from_bytes([8u8; TOKEN_BYTES])
        );
    }

    #[test]
    fn a_difference_in_any_single_byte_is_detected() {
        // The accumulator must fold every byte. If the loop stopped early, or
        // skipped the last position, a near-miss guess would be accepted — and
        // the final byte is exactly the one an incremental attacker reaches
        // last, so it gets an explicit case below.
        let base = [0u8; TOKEN_BYTES];

        for index in 0..TOKEN_BYTES {
            let mut altered = base;
            altered[index] = 1;

            assert_ne!(
                WebhookToken::from_bytes(base),
                WebhookToken::from_bytes(altered),
                "a difference at byte {index} went unnoticed"
            );
        }

        let mut last_byte_differs = base;
        last_byte_differs[TOKEN_BYTES - 1] = 1;
        assert_ne!(
            WebhookToken::from_bytes(base),
            WebhookToken::from_bytes(last_byte_differs)
        );

        // A single differing bit in the final byte is the smallest possible
        // difference, and the one an accumulator folded with the wrong
        // operator would be most likely to lose.
        let mut last_bit_differs = [0xFFu8; TOKEN_BYTES];
        last_bit_differs[TOKEN_BYTES - 1] = 0xFE;
        assert_ne!(
            WebhookToken::from_bytes([0xFFu8; TOKEN_BYTES]),
            WebhookToken::from_bytes(last_bit_differs)
        );
    }

    #[test]
    fn equality_is_reflexive_symmetric_and_consistent_with_hashing() {
        // `Hash` is hand-written alongside a hand-written `PartialEq`, so the
        // invariant the standard library relies on is asserted rather than
        // assumed: equal values must hash equally.
        fn hash_of(token: &WebhookToken) -> u64 {
            let mut hasher = DefaultHasher::new();
            token.hash(&mut hasher);
            hasher.finish()
        }

        for token in sample_tokens(64) {
            let copy = WebhookToken::from_bytes(*token.as_bytes());

            assert_eq!(token, copy);
            assert_eq!(copy, token);
            assert_eq!(hash_of(&token), hash_of(&copy));
        }

        // And the pair must actually work as a key, which is how a token is
        // looked up on the ingress path.
        let tokens = sample_tokens(128);
        let index: HashMap<WebhookToken, usize> =
            tokens.iter().copied().zip(0..tokens.len()).collect();

        assert_eq!(index.len(), tokens.len());
        for (position, token) in tokens.iter().enumerate() {
            assert_eq!(index.get(token), Some(&position));
        }
    }

    #[test]
    fn debug_output_does_not_disclose_the_token() {
        // `{:?}` is how a value reaches a log line, a panic message or an
        // error report — almost always indirectly, via a derived `Debug` on
        // some enclosing struct. A logged bearer credential is a disclosed
        // one, so the redaction has to hold through that nesting too.
        let token = sample_tokens(1).pop().unwrap();
        let encoded = token.to_string();

        let rendered = format!("{token:?}");
        assert_eq!(rendered, "WebhookToken(********)");
        assert!(!rendered.contains(&encoded));

        for nested in [
            format!("{:?}", Some(token)),
            format!("{:?}", vec![token]),
            format!("{:?}", (token, "workflow")),
        ] {
            assert!(
                !nested.contains(&encoded),
                "the token leaked through a container: {nested}"
            );
            assert!(nested.contains("WebhookToken(********)"));
        }

        // Not even a fragment: a partial disclosure shrinks the search space.
        for window in encoded.as_bytes().windows(4) {
            let fragment = std::str::from_utf8(window).unwrap();
            assert!(
                !rendered.contains(fragment),
                "the fragment {fragment} leaked"
            );
        }
    }

    #[test]
    fn display_still_renders_the_token_in_full() {
        // The redaction is on `Debug` alone. `Display` is the deliberate act of
        // producing the URL, and must not be redacted or the UI could not show
        // anyone their own webhook address.
        let token = WebhookToken::from_bytes([0x00; TOKEN_BYTES]);

        assert_eq!(token.to_string(), "AAAAAAAAAAAAAAAAAAAAAA");
        assert!(!token.to_string().contains('*'));
    }

    #[test]
    fn the_bytes_are_returned_exactly_as_supplied() {
        // Storage and key derivation both go through `as_bytes`, so any
        // normalisation here would desynchronise a stored token from the one
        // in the URL.
        let bytes = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD,
            0xFE, 0xFF,
        ];

        assert_eq!(WebhookToken::from_bytes(bytes).as_bytes(), &bytes);
    }

    #[test]
    fn error_messages_are_full_sentences_that_explain_the_rule() {
        // These reach whoever called the ingress endpoint, so they should say
        // what is expected without hinting at what the real token is.
        let wrong_length = WebhookTokenError::WrongLength {
            found: 10,
            expected: ENCODED_LEN,
        }
        .to_string();
        assert_eq!(
            wrong_length,
            "A webhook token should be 22 characters long, but 10 were provided."
        );

        let invalid = WebhookTokenError::InvalidCharacter { character: '+' }.to_string();
        assert_eq!(
            invalid,
            "A webhook token contains only letters, digits, '-' and '_', but this one contains '+'."
        );

        for message in [wrong_length, invalid] {
            assert!(message.ends_with('.'));
            assert!(message.starts_with('A'));
        }
    }
}
