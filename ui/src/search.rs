//! The unified search filter shared by the admin toolbar and the routed pages.
//!
//! A query is a space-separated list of terms. A term may be scoped to a
//! particular property using a `field:value` prefix (for example
//! `partition:cron` or `workflow:copper-tiger-canyon`); an unscoped term (for
//! example `ynab`) matches against every searchable property of an entry. All
//! terms must match (logical AND) and matching is case-insensitive and
//! substring-based.
//!
//! Which fields exist is decided by the page rather than here. Every page has
//! its own idea of what an entry is, and a fixed list meant a page either
//! borrowed names that did not describe its data or offered no scoping at all.

use std::rc::Rc;

use yew::prelude::*;

/// A property a page offers to scope a search term to.
#[derive(Clone, PartialEq)]
pub struct SearchField {
    /// The name typed before the colon.
    pub name: AttrValue,
    /// A short description, shown beside the `name:` suggestion.
    pub description: AttrValue,
    /// The values offered once a term is scoped to this field. Expected to be
    /// de-duplicated and sorted.
    pub values: Vec<AttrValue>,
}

impl SearchField {
    pub fn new(
        name: impl Into<AttrValue>,
        description: impl Into<AttrValue>,
        values: Vec<AttrValue>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            values,
        }
    }
}

/// A single parsed search term.
#[derive(Clone, PartialEq)]
struct Term {
    /// The property the term is scoped to, or `None` for a free-text term that
    /// matches against every property.
    field: Option<String>,
    /// The lowercased needle to look for.
    needle: String,
    /// The whole lowercased token. Used when the entry being matched has no
    /// such field, which is how a token that merely contains a colon (a URL,
    /// say) keeps working as plain free text.
    raw: String,
}

/// A parsed search query: a conjunction of [`Term`]s.
#[derive(Clone, PartialEq, Default)]
pub struct SearchFilter {
    terms: Vec<Term>,
}

/// The searchable properties of a single entry, supplied by the page when
/// evaluating a [`SearchFilter`].
pub struct MatchContext<'a> {
    /// The named properties this entry can be scoped by, as `(field, value)`.
    pub fields: &'a [(&'a str, &'a str)],
    /// A pre-lowercased concatenation of every searchable property, used to
    /// evaluate free-text terms.
    pub text: &'a str,
}

impl MatchContext<'_> {
    fn value_of(&self, field: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(field))
            .map(|(_, value)| *value)
    }
}

impl SearchFilter {
    /// Parses a raw query string into a [`SearchFilter`].
    pub fn parse(query: &str) -> Self {
        let terms = query
            .split_whitespace()
            .map(|token| {
                let raw = token.to_lowercase();
                match token.split_once(':') {
                    Some((field, value)) if !field.is_empty() && !value.is_empty() => Term {
                        field: Some(field.to_lowercase()),
                        needle: value.to_lowercase(),
                        raw,
                    },
                    _ => Term {
                        field: None,
                        needle: raw.clone(),
                        raw,
                    },
                }
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
            Some(field) => match ctx.value_of(field) {
                Some(value) => value.to_lowercase().contains(&term.needle),
                None => ctx.text.contains(&term.raw),
            },
            None => ctx.text.contains(&term.needle),
        })
    }
}

/// The shared search state provided to the toolbar (which owns the input) and
/// the routed page (which consumes the parsed filter).
#[derive(Clone, PartialEq)]
pub struct SearchContext {
    /// The raw query string, bound to the toolbar's search input.
    pub query: AttrValue,
    /// The parsed query, shared so consumers don't re-parse it.
    pub filter: Rc<SearchFilter>,
    /// Replaces the current query string.
    pub set: Callback<String>,
}

/// The fields available for completion of scoped search terms (for example the
/// partition names offered after typing `partition:`). It is published by the
/// page that owns the data and consumed by the toolbar's autocomplete.
#[derive(Clone, PartialEq, Default)]
pub struct SearchVocabulary {
    /// The fields this page offers, in the order they are presented.
    pub fields: Vec<SearchField>,
}

impl SearchVocabulary {
    pub fn new(fields: Vec<SearchField>) -> Self {
        Self { fields }
    }

    /// The candidate values for a scoped term's field, or `None` when the page
    /// does not offer that field.
    pub fn values_for(&self, field: &str) -> Option<&[AttrValue]> {
        self.fields
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(field))
            .map(|candidate| candidate.values.as_slice())
    }
}

/// The shared completion vocabulary, provided above both the toolbar (which
/// reads it to suggest values) and the routed page (which publishes it from the
/// loaded data).
#[derive(Clone, PartialEq)]
pub struct VocabularyContext {
    /// The current vocabulary, shared so the app bar can read it cheaply.
    pub vocabulary: Rc<SearchVocabulary>,
    /// Replaces the published vocabulary.
    pub set: Callback<SearchVocabulary>,
}
