//! The unified search filter shared by the admin app bar and the partition
//! browser.
//!
//! A query is a space-separated list of terms. A term may be scoped to a
//! particular property using a `field:value` prefix (for example
//! `partition:cron` or `key:ynab`); an unscoped term (for example `ynab`)
//! matches against every searchable property of an entry. All terms must match
//! (logical AND) and matching is case-insensitive and substring-based.

use std::rc::Rc;

use yew::prelude::*;

/// The canonical `field:` prefixes offered as autocomplete suggestions in the
/// search input, paired with a short human description. The order here is the
/// order they are presented to the user.
pub const FIELD_PREFIXES: &[(&str, &str)] = &[
    ("partition:", "Match a partition name"),
    ("key:", "Match an entry key"),
    ("kind:", "Match the store kind (kv or queue)"),
];

/// A property a search term can be scoped to.
#[derive(Clone, PartialEq)]
enum Field {
    Partition,
    Key,
    Kind,
}

/// A single parsed search term.
#[derive(Clone, PartialEq)]
struct Term {
    /// The property the term is scoped to, or `None` for a free-text term that
    /// matches against every property.
    field: Option<Field>,
    /// The lowercased needle to look for.
    needle: String,
}

/// A parsed search query: a conjunction of [`Term`]s.
#[derive(Clone, PartialEq, Default)]
pub struct SearchFilter {
    terms: Vec<Term>,
}

/// The searchable properties of a single entry, supplied by the browser when
/// evaluating a [`SearchFilter`].
pub struct MatchContext<'a> {
    /// The partition the entry belongs to.
    pub partition: &'a str,
    /// The entry's key within its partition.
    pub key: &'a str,
    /// The entry's store kind (for example `kv` or `queue`).
    pub kind: &'a str,
    /// A pre-lowercased concatenation of every searchable property (partition,
    /// key, kind, and payload), used to evaluate free-text terms.
    pub text: &'a str,
}

impl SearchFilter {
    /// Parses a raw query string into a [`SearchFilter`].
    ///
    /// A `field:value` token is only treated as scoped when `field` is a known
    /// property name; this keeps tokens that merely contain a colon (such as a
    /// URL key) working as plain free-text terms.
    pub fn parse(query: &str) -> Self {
        let terms = query
            .split_whitespace()
            .filter_map(|token| {
                if let Some((prefix, value)) = token.split_once(':') {
                    let field = match prefix.to_ascii_lowercase().as_str() {
                        "partition" | "p" => Some(Field::Partition),
                        "key" | "k" => Some(Field::Key),
                        "kind" | "type" => Some(Field::Kind),
                        _ => None,
                    };
                    if let Some(field) = field {
                        if value.is_empty() {
                            return None;
                        }
                        return Some(Term {
                            field: Some(field),
                            needle: value.to_lowercase(),
                        });
                    }
                }
                Some(Term {
                    field: None,
                    needle: token.to_lowercase(),
                })
            })
            .collect();
        Self { terms }
    }

    /// Returns true when the query carries no terms (matches everything).
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Evaluates the filter against a single entry's properties.
    pub fn matches(&self, ctx: &MatchContext) -> bool {
        self.terms.iter().all(|term| match &term.field {
            Some(Field::Partition) => ctx.partition.to_lowercase().contains(&term.needle),
            Some(Field::Key) => ctx.key.to_lowercase().contains(&term.needle),
            Some(Field::Kind) => ctx.kind.to_lowercase().contains(&term.needle),
            None => ctx.text.contains(&term.needle),
        })
    }
}

/// The shared search state provided to the app bar (which owns the input) and
/// the partition browser (which consumes the parsed filter).
#[derive(Clone, PartialEq)]
pub struct SearchContext {
    /// The raw query string, bound to the app bar's search input.
    pub query: AttrValue,
    /// The parsed query, shared so consumers don't re-parse it.
    pub filter: Rc<SearchFilter>,
    /// Replaces the current query string.
    pub set: Callback<String>,
}

/// The concrete values available for context-aware completion of scoped search
/// terms (for example the partition names offered after typing `partition:`).
/// It is published by the page that owns the data and consumed by the app bar's
/// autocomplete. Each list is expected to be de-duplicated and sorted.
#[derive(Clone, PartialEq, Default)]
pub struct SearchVocabulary {
    /// Every known partition name.
    pub partitions: Vec<AttrValue>,
    /// Every known entry key (across all partitions).
    pub keys: Vec<AttrValue>,
    /// Every known store kind (for example `kv` and `queue`).
    pub kinds: Vec<AttrValue>,
}

impl SearchVocabulary {
    /// Returns the candidate values for a scoped term's field, accepting the
    /// same field names and aliases as the parser. Returns `None` when the
    /// field is unknown (so no value completion is offered).
    pub fn values_for(&self, field: &str) -> Option<&[AttrValue]> {
        match field.to_ascii_lowercase().as_str() {
            "partition" | "p" => Some(&self.partitions),
            "key" | "k" => Some(&self.keys),
            "kind" | "type" => Some(&self.kinds),
            _ => None,
        }
    }
}

/// The shared completion vocabulary, provided above both the app bar (which
/// reads it to suggest values) and the routed page (which publishes it from the
/// loaded data).
#[derive(Clone, PartialEq)]
pub struct VocabularyContext {
    /// The current vocabulary, shared so the app bar can read it cheaply.
    pub vocabulary: Rc<SearchVocabulary>,
    /// Replaces the published vocabulary.
    pub set: Callback<SearchVocabulary>,
}

