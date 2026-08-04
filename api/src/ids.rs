//! Human-memorable identifiers.
//!
//! Every user-visible entity in Automate — workflows, connections — is
//! identified by a short sequence of ordinary English words rather than an
//! opaque UUID, so that people can read an identifier off a screen, say it out
//! loud, and type it back in without transcription errors.
//!
//! ```text
//! copper-tiger-canyon     a workflow
//! brisk-harbor            a connection
//! ```
//!
//! # Sizing
//!
//! The wordlist holds exactly 2048 entries, so each word carries 11 bits. That
//! fixes the size of the identifier space:
//!
//! | Words | Bits | Distinct identifiers |
//! |-------|------|----------------------|
//! | 2     | 22   | 4,194,304            |
//! | 3     | 33   | 8,589,934,592        |
//! | 4     | 44   | 17,592,186,044,416   |
//!
//! A `u64` cannot be represented in two or three words — it needs six, which is
//! well past the point of being memorable. So rather than encoding an existing
//! 64-bit identifier, the word count *defines* the identifier space, and the
//! `u64` is simply the widest integer that comfortably holds it. Widening an
//! identifier later is a change to its word count, not to its type.
//!
//! Identifiers are scoped to a tenant, so the collision domain is one user's
//! handful of workflows rather than the whole installation. At that scale 33
//! bits is enormous; identifiers are generated randomly and retried on the
//! (vanishingly rare) collision.
//!
//! # Not a secret
//!
//! A three-word identifier is memorable precisely because it is short, which
//! also makes it enumerable. Identifiers are names, never credentials. Anything
//! that needs to be unguessable — a webhook ingress URL, for instance — carries
//! its own high-entropy token alongside its identifier.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::wordlist::WORDS;

/// The number of bits carried by a single word: `log2(2048)`.
pub const BITS_PER_WORD: u32 = 11;

/// The largest number of words a [`WordId`] can hold, bounded by the 64 bits of
/// its backing integer (`5 * 11 = 55`, whereas `6 * 11 = 66` would not fit).
pub const MAX_WORDS: usize = 5;

/// The bit mask selecting a single word's worth of bits.
const WORD_MASK: u64 = (1 << BITS_PER_WORD) - 1;

/// The character used to join words when rendering an identifier.
pub const SEPARATOR: char = '-';

/// The identifier of a workflow, rendered as three words.
pub type WorkflowId = WordId<3>;

/// The identifier of a connection, rendered as two words.
pub type ConnectionId = WordId<2>;

/// An identifier encoded as `N` words drawn from the BIP-39 English wordlist.
///
/// The encoding is lossless in both directions: every value in `0..=Self::MAX`
/// maps to exactly one word sequence and vice versa. Words are emitted
/// most-significant first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WordId<const N: usize>(u64);

impl<const N: usize> WordId<N> {
    /// Rejects word counts that cannot round-trip through a `u64`.
    ///
    /// This is a post-monomorphisation check: it fires when a `WordId<N>` with
    /// an unusable `N` is actually instantiated somewhere.
    const ASSERT_SUPPORTED_WIDTH: () = assert!(
        N >= 1 && N <= MAX_WORDS,
        "WordId supports between 1 and 5 words, because a u64 holds at most five 11-bit words"
    );

    /// The number of bits this identifier carries.
    pub const BITS: u32 = BITS_PER_WORD * N as u32;

    /// The largest value this identifier can represent.
    pub const MAX: u64 = if Self::BITS >= 64 {
        u64::MAX
    } else {
        (1u64 << Self::BITS) - 1
    };

    /// The number of distinct identifiers in this space.
    pub const SPACE: u128 = Self::MAX as u128 + 1;

    /// Wraps a raw value, which must be within `0..=Self::MAX`.
    pub fn new(value: u64) -> Result<Self, WordIdError> {
        let () = Self::ASSERT_SUPPORTED_WIDTH;

        if value > Self::MAX {
            return Err(WordIdError::OutOfRange {
                value,
                max: Self::MAX,
                words: N,
            });
        }

        Ok(Self(value))
    }

    /// Derives an identifier from a source of randomness.
    ///
    /// The entropy is reduced to the low [`Self::BITS`] bits. Because that is a
    /// contiguous bit slice, a uniformly random input yields a uniformly random
    /// identifier — so callers can pass a raw CSPRNG word directly.
    ///
    /// Identifiers must be unpredictable rather than sequential: a counter
    /// would leak how many entities exist and let anyone guess a neighbour's
    /// identifier.
    pub fn from_entropy(entropy: u64) -> Self {
        let () = Self::ASSERT_SUPPORTED_WIDTH;

        Self(entropy & Self::MAX)
    }

    /// The underlying value.
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// The words making up this identifier, most-significant first.
    pub fn words(self) -> [&'static str; N] {
        core::array::from_fn(|i| {
            let shift = BITS_PER_WORD * (N - 1 - i) as u32;
            WORDS[((self.0 >> shift) & WORD_MASK) as usize]
        })
    }
}

impl<const N: usize> fmt::Display for WordId<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, word) in self.words().into_iter().enumerate() {
            if i > 0 {
                f.write_str(SEPARATOR.encode_utf8(&mut [0u8; 4]))?;
            }

            f.write_str(word)?;
        }

        Ok(())
    }
}

impl<const N: usize> fmt::Debug for WordId<N> {
    /// Renders the words rather than the raw integer, since the words are what
    /// appear in URLs, logs and the UI.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WordId({self})")
    }
}

impl<const N: usize> FromStr for WordId<N> {
    type Err = WordIdError;

    /// Parses an identifier, tolerating the ways people naturally retype one.
    ///
    /// Case is ignored, and any of `-`, `_`, `.`, or whitespace is accepted as a
    /// separator, so a value copied out of a URL, a log line or a chat message
    /// parses without cleanup.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let () = Self::ASSERT_SUPPORTED_WIDTH;

        let parts: Vec<&str> = s
            .split(|c: char| c == '-' || c == '_' || c == '.' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .collect();

        if parts.len() != N {
            return Err(WordIdError::WrongWordCount {
                expected: N,
                found: parts.len(),
            });
        }

        let mut value = 0u64;
        for part in parts {
            let word = part.to_ascii_lowercase();
            let index =
                WORDS
                    .binary_search(&word.as_str())
                    .map_err(|_| WordIdError::UnknownWord {
                        word: word.clone(),
                        suggestion: suggest(&word),
                    })?;

            value = (value << BITS_PER_WORD) | index as u64;
        }

        Ok(Self(value))
    }
}

impl<const N: usize> Serialize for WordId<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de, const N: usize> Deserialize<'de> for WordId<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WordIdVisitor<const N: usize>;

        impl<const N: usize> de::Visitor<'_> for WordIdVisitor<N> {
            type Value = WordId<N>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "an identifier of {N} hyphen-separated words")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse().map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_str(WordIdVisitor::<N>)
    }
}

/// Suggests the wordlist entry someone most likely meant.
///
/// Every word in the list is uniquely identified by its first four letters, so
/// a single mistyped or misremembered trailing character still resolves to
/// exactly one candidate.
fn suggest(word: &str) -> Option<&'static str> {
    let prefix: String = word.chars().take(4).collect();

    // Below three characters there is not enough signal to guess usefully, and
    // we would rather say nothing than send someone down the wrong path.
    if prefix.len() < 3 {
        return None;
    }

    WORDS
        .iter()
        .copied()
        .find(|candidate| candidate.starts_with(&prefix) || prefix.starts_with(candidate))
}

/// The ways parsing or constructing a [`WordId`] can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordIdError {
    /// The input had a different number of words than the identifier expects.
    WrongWordCount { expected: usize, found: usize },

    /// A word was not in the wordlist.
    UnknownWord {
        word: String,
        suggestion: Option<&'static str>,
    },

    /// A raw value was too large for the identifier's word count.
    OutOfRange { value: u64, max: u64, words: usize },
}

impl fmt::Display for WordIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongWordCount { expected, found } => write!(
                f,
                "This identifier should be made up of {expected} words, but {found} were provided."
            ),
            Self::UnknownWord {
                word,
                suggestion: Some(suggestion),
            } => write!(
                f,
                "'{word}' is not a recognised identifier word. Did you mean '{suggestion}'?"
            ),
            Self::UnknownWord {
                word,
                suggestion: None,
            } => write!(f, "'{word}' is not a recognised identifier word."),
            Self::OutOfRange { value, max, words } => write!(
                f,
                "The value {value} cannot be represented in {words} words, which hold at most {max}."
            ),
        }
    }
}

impl std::error::Error for WordIdError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wordlist::WORD_COUNT;

    #[test]
    fn the_wordlist_has_the_properties_the_encoding_relies_on() {
        assert_eq!(WORD_COUNT, 1 << BITS_PER_WORD);
        assert!(
            WORDS.windows(2).all(|pair| pair[0] < pair[1]),
            "the wordlist must be sorted and free of duplicates so it can be binary searched"
        );
    }

    #[test]
    fn width_constants_follow_from_the_word_count() {
        assert_eq!(WordId::<2>::BITS, 22);
        assert_eq!(WordId::<3>::BITS, 33);
        assert_eq!(WordId::<2>::MAX, 4_194_303);
        assert_eq!(WordId::<3>::MAX, 8_589_934_591);
        assert_eq!(WordId::<3>::SPACE, 8_589_934_592);
    }

    #[test]
    fn the_extremes_of_the_space_encode_to_the_extremes_of_the_wordlist() {
        assert_eq!(
            WordId::<3>::new(0).unwrap().to_string(),
            "abandon-abandon-abandon"
        );
        assert_eq!(
            WordId::<3>::new(WordId::<3>::MAX).unwrap().to_string(),
            "zoo-zoo-zoo"
        );
    }

    #[test]
    fn every_single_word_value_round_trips() {
        // Exhaustive across the whole 11-bit space of a one-word identifier,
        // which transitively covers every entry in the wordlist.
        for value in 0..=WordId::<1>::MAX {
            let id = WordId::<1>::new(value).unwrap();
            let rendered = id.to_string();
            let parsed: WordId<1> = rendered.parse().unwrap();

            assert_eq!(parsed, id, "{rendered} did not round-trip");
            assert_eq!(parsed.as_u64(), value);
        }
    }

    #[test]
    fn three_word_values_round_trip_across_the_space() {
        // Striding with a prime keeps the sample spread across all three word
        // positions rather than exercising only the low bits.
        let mut value = 0u64;
        while value <= WordId::<3>::MAX {
            let id = WordId::<3>::new(value).unwrap();
            let parsed: WordId<3> = id.to_string().parse().unwrap();

            assert_eq!(parsed.as_u64(), value);
            value += 1_000_003;
        }

        let max = WordId::<3>::new(WordId::<3>::MAX).unwrap();
        assert_eq!(max.to_string().parse::<WordId<3>>().unwrap(), max);
    }

    #[test]
    fn words_are_emitted_most_significant_first() {
        // 1 << 22 sets only the topmost word, so only the first word moves.
        let id = WordId::<3>::new(1 << 22).unwrap();
        assert_eq!(id.words(), ["ability", "abandon", "abandon"]);

        // 1 sets only the bottom word.
        let id = WordId::<3>::new(1).unwrap();
        assert_eq!(id.words(), ["abandon", "abandon", "ability"]);
    }

    #[test]
    fn parsing_tolerates_the_ways_people_retype_an_identifier() {
        let expected: WordId<3> = "copper-tiger-canyon".parse().unwrap();

        for input in [
            "copper-tiger-canyon",
            "copper_tiger_canyon",
            "copper tiger canyon",
            "copper.tiger.canyon",
            "COPPER-TIGER-CANYON",
            "Copper-Tiger-Canyon",
            "  copper-tiger-canyon  ",
            "copper--tiger---canyon",
        ] {
            assert_eq!(
                input.parse::<WordId<3>>().unwrap(),
                expected,
                "failed on {input:?}"
            );
        }
    }

    #[test]
    fn the_wrong_number_of_words_is_reported_with_both_counts() {
        let err = "copper-tiger".parse::<WordId<3>>().unwrap_err();
        assert_eq!(
            err,
            WordIdError::WrongWordCount {
                expected: 3,
                found: 2
            }
        );
        assert!(err.to_string().contains("3 words"));
    }

    #[test]
    fn a_mistyped_word_suggests_the_intended_one() {
        // A trailing typo leaves the first four letters intact, which the
        // wordlist guarantees is enough to identify the word uniquely.
        let err = "coppex-tiger-canyon".parse::<WordId<3>>().unwrap_err();
        let WordIdError::UnknownWord { word, suggestion } = err else {
            panic!("expected an unknown word error, got {err:?}");
        };

        assert_eq!(word, "coppex");
        assert_eq!(suggestion, Some("copper"));
        assert!(err_display(&word, suggestion).contains("Did you mean 'copper'?"));
    }

    #[test]
    fn a_word_with_no_plausible_match_suggests_nothing() {
        let err = "xyzzy-tiger-canyon".parse::<WordId<3>>().unwrap_err();
        let WordIdError::UnknownWord { suggestion, .. } = err else {
            panic!("expected an unknown word error");
        };

        assert_eq!(suggestion, None);
    }

    fn err_display(word: &str, suggestion: Option<&'static str>) -> String {
        WordIdError::UnknownWord {
            word: word.to_string(),
            suggestion,
        }
        .to_string()
    }

    #[test]
    fn values_beyond_the_space_are_rejected_rather_than_truncated() {
        let err = WordId::<2>::new(WordId::<2>::MAX + 1).unwrap_err();
        assert!(matches!(err, WordIdError::OutOfRange { words: 2, .. }));
    }

    #[test]
    fn entropy_is_reduced_into_the_space_rather_than_rejected() {
        for entropy in [0, 1, u64::MAX, u64::MAX / 3, 0xDEAD_BEEF_CAFE_F00D] {
            let id = WordId::<3>::from_entropy(entropy);
            assert!(id.as_u64() <= WordId::<3>::MAX);
            assert_eq!(id.to_string().parse::<WordId<3>>().unwrap(), id);
        }

        // The low bits are preserved exactly, which is what makes a uniformly
        // random input yield a uniformly random identifier.
        assert_eq!(
            WordId::<3>::from_entropy(u64::MAX).as_u64(),
            WordId::<3>::MAX
        );
        assert_eq!(WordId::<3>::from_entropy(12345).as_u64(), 12345);
    }

    #[test]
    fn identifiers_serialise_as_their_word_form() {
        let id = WordId::<3>::new(123_456_789).unwrap();
        let json = serde_json::to_string(&id).unwrap();

        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<WordId<3>>(&json).unwrap(), id);
    }

    #[test]
    fn deserialising_a_malformed_identifier_explains_the_problem() {
        let err = serde_json::from_str::<WordId<3>>("\"copper-tiger\"").unwrap_err();
        assert!(
            err.to_string().contains("3 words"),
            "unhelpful error: {err}"
        );
    }

    #[test]
    fn debug_output_shows_the_words() {
        let id = WordId::<2>::new(0).unwrap();
        assert_eq!(format!("{id:?}"), "WordId(abandon-abandon)");
    }
}
