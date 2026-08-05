//! Filtering and templating over the raw JSON body of a webhook delivery.
//!
//! Most webhook workflows in this agent are built around a typed struct: we
//! know what Sentry sends, so we deserialize it and work with real fields. That
//! stops working the moment somebody wants to point Automate at a service we
//! have never heard of, or at one of the several hundred fields GitHub sends
//! which we did not model. Writing a new Rust struct is not something a user
//! can do, so the alternative is to let them address the payload directly.
//!
//! This module provides the two halves of that:
//!
//! * [`JsonFilter`] exposes an arbitrary [`serde_json::Value`] to the filter
//!   DSL, so a user can write `issue.user.login == "octocat"` to decide whether
//!   a delivery is interesting.
//! * [`render`] expands `${{ issue.title }}` inside a user-supplied template, so
//!   the same user can say what the resulting task should be called.
//!
//! # One addressing syntax, one interpolation syntax
//!
//! Both halves resolve a dotted path against the payload, and [`render`] is
//! built on [`crate::parsers::interpolate`] rather than on a template engine of
//! its own. That is deliberate: the configuration file already uses `${{ ... }}`
//! for environment substitution, and a product which spells the same idea two
//! different ways in two different places makes its users guess. The cost is
//! that this module is very nearly a thin adapter, which is the point.
//!
//! # What the payload is trusted to do
//!
//! A webhook body is supplied by whoever can reach the endpoint, so it is
//! treated as attacker-influenceable throughout: absent fields degrade quietly
//! instead of failing the delivery, and rendered output is length-capped so that
//! a template aimed at a large field cannot produce an unbounded task title.

// The webhook workflow that consumes this is wired up separately; the module is
// written as a complete, self-contained capability in the same spirit as the
// storage traits in `crate::db`.
#![allow(dead_code)]

use crate::filter::{FilterValue, Filterable, json_to_filter_value};

/// The largest rendered string [`render`] will return, in characters.
///
/// A webhook body is attacker-influenceable: anybody who can reach the endpoint
/// chooses what is in it. A template such as `${{ commits }}` against a large
/// push event, or a deliberately padded field, would otherwise flow straight
/// into a task title and from there into the database, the API responses and
/// the UI. Capping the output keeps a hostile (or merely enthusiastic) payload
/// from turning one delivery into an unbounded record.
const MAX_RENDERED_LENGTH: usize = 8192;

/// A [`Filterable`] view over an arbitrary JSON document, addressing it with
/// dotted paths such as `issue.user.login`.
///
/// # Why the whole path arrives as one key
///
/// The filter DSL's lexer treats `.`-separated identifiers as a single property
/// name, so [`Filterable::get`] is called once with `"issue.user.login"` rather
/// than three times. There is therefore nothing clever to do here: split on `.`
/// and walk the document, yielding [`FilterValue::Null`] the moment a segment is
/// missing. That matches the DSL's own convention, where an unknown property is
/// null rather than an error.
///
/// # Known limitation: object leaves are not filterable
///
/// [`json_to_filter_value`] maps a JSON object onto [`FilterValue::Null`],
/// because the filter DSL has no representation for a structured record — it
/// knows about strings, numbers, booleans and tuples, and nothing else. A path
/// which stops at an object therefore evaluates to null: `issue` on its own is
/// not filterable, but `issue.title` is. Users must address the scalar they
/// actually care about.
///
/// Arrays fare better. They become [`FilterValue::Tuple`]s, which is exactly
/// what the DSL's `in` and `contains` operators consume, so a filter can be
/// written against a list of labels or reviewers:
///
/// ```text
/// "bug" in issue.labels
/// ```
///
/// Note that an array *of objects* becomes a tuple of nulls, for the same
/// reason as above.
pub struct JsonFilter<'a>(pub &'a serde_json::Value);

impl Filterable for JsonFilter<'_> {
    fn get(&self, key: &str) -> FilterValue<'_> {
        match resolve(self.0, key) {
            // Borrow the leaf rather than cloning it, so evaluating a filter
            // over a large payload stays allocation-free for scalars.
            Some(value) => json_to_filter_value(value),
            None => FilterValue::Null,
        }
    }
}

/// Expands `${{ path }}` expressions in `template` against `payload`.
///
/// The expression is a dotted path into the payload, trimmed of surrounding
/// whitespace, so `${{ issue.title }}` and `${{issue.title}}` are the same
/// thing. Escaping works exactly as it does elsewhere in the product: a
/// `\${{ ... }}` is emitted literally and its contents are never resolved.
///
/// # Why a missing path is not an error
///
/// A webhook payload is not a schema, it is whatever the sender chose to
/// include *this time*. The same GitHub event carries `issue.assignee` on one
/// delivery and omits it on the next; optional fields appear and disappear
/// between API versions and between plan tiers. If an absent field failed the
/// render, it would fail the task, and the user would lose the notification
/// entirely — a cosmetic gap in a title escalated into a silently dropped
/// alert. Rendering the empty string keeps the notification, which is the part
/// the user actually needed.
///
/// An explicit JSON `null` is treated the same way as an absent field, since
/// senders are inconsistent about which they use for "no value" and the user
/// means the same thing by both. The alternative would be to write the literal
/// text `null` into a task title.
///
/// # How values are rendered
///
/// A string leaf is emitted without its JSON quotes, which is what a user
/// writing a title expects. Other scalars are emitted in their JSON form
/// (`42`, `true`). An object or array is emitted as compact JSON, which is
/// rarely what someone wants in a title but is far more useful than nothing
/// when they are debugging what a sender actually posts.
///
/// The result is truncated to [`MAX_RENDERED_LENGTH`] characters with a
/// trailing `…` if it would otherwise be longer.
pub fn render(template: &str, payload: &serde_json::Value) -> Result<String, human_errors::Error> {
    let rendered = crate::parsers::interpolate(template, |expression| {
        Ok(render_leaf(resolve(payload, expression.trim())))
    })?;

    Ok(truncate(rendered))
}

/// Walks a dotted path into a JSON document, returning [`None`] as soon as a
/// segment is not present.
///
/// Segments address object keys only. Array elements are deliberately not
/// indexable: the filter DSL consumes whole arrays as tuples, and a template
/// reaching for `commits.0` would be depending on an ordering the sender never
/// promised.
fn resolve<'a>(payload: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = payload;

    for segment in path.split('.') {
        current = current.as_object()?.get(segment)?;
    }

    Some(current)
}

/// Converts a resolved leaf into the text that should appear in the output.
fn render_leaf(value: Option<&serde_json::Value>) -> String {
    match value {
        // Absent and explicitly-null both mean "the sender had nothing to say".
        None | Some(serde_json::Value::Null) => String::new(),
        // Emitted bare, because `"Fix the bug"` in a task title is noise.
        Some(serde_json::Value::String(text)) => text.clone(),
        // `Display` for `Value` is compact JSON, which is what we want for
        // numbers, booleans, objects and arrays alike.
        Some(other) => other.to_string(),
    }
}

/// Caps a rendered string at [`MAX_RENDERED_LENGTH`] characters, marking any
/// truncation with a trailing `…` so the reader can tell something was cut.
fn truncate(rendered: String) -> String {
    // A UTF-8 string is never shorter in bytes than it is in characters, so the
    // cheap byte check safely short-circuits the common case without walking
    // the whole string.
    if rendered.len() <= MAX_RENDERED_LENGTH && rendered.chars().count() <= MAX_RENDERED_LENGTH {
        return rendered;
    }

    // The ellipsis is counted against the budget, so the returned string never
    // exceeds the advertised limit. Truncating by characters rather than bytes
    // also keeps us from slicing a multi-byte codepoint in half.
    let mut truncated: String = rendered.chars().take(MAX_RENDERED_LENGTH - 1).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Filter;

    /// A payload shaped like the parts of a GitHub issue event people actually
    /// write filters and titles against.
    fn payload() -> serde_json::Value {
        serde_json::json!({
            "action": "opened",
            "number": 42,
            "draft": false,
            "issue": {
                "title": "Everything is on fire",
                "labels": ["bug", "urgent"],
                "assignee": null,
                "user": {
                    "login": "octocat"
                }
            }
        })
    }

    #[test]
    fn a_nested_path_resolves_to_its_leaf() {
        // The lexer hands us the entire dotted path as one key, so this is the
        // property that proves we walk it rather than looking it up verbatim.
        let payload = payload();
        let filter = JsonFilter(&payload);

        assert_eq!(filter.get("issue.user.login"), FilterValue::from("octocat"));
        assert_eq!(filter.get("action"), FilterValue::from("opened"));
        assert_eq!(filter.get("number"), FilterValue::Number(42.0));
        assert_eq!(filter.get("draft"), FilterValue::Bool(false));
    }

    #[test]
    fn a_missing_path_is_null_rather_than_a_failure() {
        // Payloads vary between deliveries, so an unknown path has to behave
        // like any other unknown property in the DSL: null, not an error. A
        // filter that referenced a field the sender omitted would otherwise
        // take down the whole delivery.
        let payload = payload();
        let filter = JsonFilter(&payload);

        assert_eq!(filter.get("issue.milestone"), FilterValue::Null);
        assert_eq!(filter.get("issue.user.email"), FilterValue::Null);
        assert_eq!(filter.get("nonexistent"), FilterValue::Null);
        // A path that tries to walk *through* a scalar is missing, not an error.
        assert_eq!(filter.get("action.length"), FilterValue::Null);
        // An explicit JSON null is indistinguishable from an absent field here,
        // which is what a user filtering on `issue.assignee == null` expects.
        assert_eq!(filter.get("issue.assignee"), FilterValue::Null);
    }

    #[test]
    fn an_array_leaf_becomes_a_tuple_the_dsl_can_match_against() {
        // Arrays are the one structured shape the DSL understands, and this is
        // the whole reason it is worth mapping them: membership tests over
        // labels, reviewers and tags are the common case for webhook filters.
        // Exercised through a real parsed filter so that a change in either the
        // conversion or the DSL's tuple handling is caught here.
        let payload = payload();
        let filter = JsonFilter(&payload);

        assert_eq!(
            filter.get("issue.labels"),
            FilterValue::Tuple(vec![FilterValue::from("bug"), FilterValue::from("urgent")])
        );

        assert!(
            Filter::new("\"bug\" in issue.labels")
                .expect("parse filter")
                .matches(&filter)
                .expect("run filter")
        );

        assert!(
            !Filter::new("\"wontfix\" in issue.labels")
                .expect("parse filter")
                .matches(&filter)
                .expect("run filter")
        );

        assert!(
            Filter::new("issue.labels contains \"urgent\" && issue.user.login == \"octocat\"")
                .expect("parse filter")
                .matches(&filter)
                .expect("run filter")
        );
    }

    #[test]
    fn an_object_leaf_is_null_because_the_dsl_has_no_record_type() {
        // The documented limitation, pinned by a test so it cannot quietly
        // change: `issue` is not filterable, `issue.title` is. If this ever
        // starts returning something else, the doc comment on `JsonFilter` is
        // lying to users about how to write their filters.
        let payload = payload();
        let filter = JsonFilter(&payload);

        assert_eq!(filter.get("issue"), FilterValue::Null);
        assert_eq!(filter.get("issue.user"), FilterValue::Null);
        assert_eq!(
            filter.get("issue.title"),
            FilterValue::from("Everything is on fire")
        );
    }

    #[test]
    fn a_simple_expression_is_substituted() {
        assert_eq!(
            render("Issue ${{ action }}", &payload()).expect("render template"),
            "Issue opened"
        );
    }

    #[test]
    fn a_nested_expression_is_substituted() {
        // Templates and filters have to agree on how a path is spelled, or
        // users will get one working and be baffled by the other.
        assert_eq!(
            render(
                "${{ issue.title }} (by ${{ issue.user.login }})",
                &payload()
            )
            .expect("render template"),
            "Everything is on fire (by octocat)"
        );
    }

    #[test]
    fn a_missing_expression_renders_as_empty_rather_than_failing() {
        // The important property in the whole module: an optional field the
        // sender omitted must not cost the user their notification. The
        // surrounding literal text still renders, so the task remains useful.
        assert_eq!(
            render("Assigned to '${{ issue.assignee }}'", &payload()).expect("render template"),
            "Assigned to ''"
        );
        assert_eq!(
            render("[${{ repository.full_name }}] ${{ action }}", &payload())
                .expect("render template"),
            "[] opened"
        );
    }

    #[test]
    fn a_string_leaf_renders_without_its_json_quotes() {
        // Naively reaching for `serde_json::to_string` would produce
        // `"Everything is on fire"`, quotes and all, in every task title.
        let rendered = render("${{ issue.title }}", &payload()).expect("render template");

        assert_eq!(rendered, "Everything is on fire");
        assert!(!rendered.contains('"'));
    }

    #[test]
    fn non_string_scalars_render_in_their_json_form() {
        assert_eq!(
            render("#${{ number }} draft=${{ draft }}", &payload()).expect("render template"),
            "#42 draft=false"
        );
    }

    #[test]
    fn an_object_or_array_renders_as_compact_json() {
        // Unlike the filter, a template has somewhere to put a structured
        // value, and showing it beats showing nothing when a user is working
        // out what a sender actually posts.
        assert_eq!(
            render("${{ issue.labels }}", &payload()).expect("render template"),
            r#"["bug","urgent"]"#
        );
        assert_eq!(
            render("${{ issue.user }}", &payload()).expect("render template"),
            r#"{"login":"octocat"}"#
        );
    }

    #[test]
    fn output_at_the_limit_is_left_alone_but_longer_output_is_truncated() {
        // A webhook body is attacker-influenceable, so the cap is a boundary
        // worth pinning exactly rather than approximately.
        let exact = serde_json::json!({ "blob": "x".repeat(MAX_RENDERED_LENGTH) });
        let rendered = render("${{ blob }}", &exact).expect("render template");
        assert_eq!(rendered.chars().count(), MAX_RENDERED_LENGTH);
        assert!(!rendered.ends_with('…'));

        let oversized = serde_json::json!({ "blob": "x".repeat(MAX_RENDERED_LENGTH + 1) });
        let rendered = render("${{ blob }}", &oversized).expect("render template");
        assert_eq!(rendered.chars().count(), MAX_RENDERED_LENGTH);
        assert!(rendered.ends_with('…'));

        let huge = serde_json::json!({ "blob": "x".repeat(MAX_RENDERED_LENGTH * 10) });
        let rendered = render("${{ blob }}", &huge).expect("render template");
        assert_eq!(rendered.chars().count(), MAX_RENDERED_LENGTH);
        assert!(rendered.ends_with('…'));
    }

    #[test]
    fn truncation_counts_characters_so_it_cannot_split_a_codepoint() {
        // Truncating by bytes would panic on a multi-byte boundary, and a
        // webhook payload is exactly the sort of input that carries emoji and
        // non-Latin text.
        let payload = serde_json::json!({ "blob": "é".repeat(MAX_RENDERED_LENGTH + 100) });
        let rendered = render("${{ blob }}", &payload).expect("render template");

        assert_eq!(rendered.chars().count(), MAX_RENDERED_LENGTH);
        assert!(rendered.ends_with('…'));
        assert!(rendered.starts_with('é'));
    }

    #[test]
    fn an_escaped_expression_is_passed_through_literally() {
        // Matches how `parsers::interpolation` already behaves: the backslash
        // is dropped, the `$` is emitted, and the braces that follow are then
        // ordinary text — so the expression is never resolved. A user who wants
        // a literal `${{ ... }}` in a title has one way to ask for it, and it
        // is the same way they ask for it in the configuration file.
        assert_eq!(
            render(r"\${{ issue.title }}", &payload()).expect("render template"),
            "${{ issue.title }}"
        );
        assert_eq!(
            render(r"${{ action }} but not \${{ action }}", &payload()).expect("render template"),
            "opened but not ${{ action }}"
        );
        // The escape does not need the path to exist, because it never resolves
        // one.
        assert_eq!(
            render(r"\${{ totally.unknown }}", &payload()).expect("render template"),
            "${{ totally.unknown }}"
        );
    }

    #[test]
    fn a_template_without_expressions_is_returned_unchanged() {
        assert_eq!(
            render("A fixed title", &payload()).expect("render template"),
            "A fixed title"
        );
    }

    #[test]
    fn an_unclosed_expression_is_still_an_error() {
        // Missing *data* is tolerated; a malformed *template* is not, because
        // that is a mistake the user made and can fix, and reporting it is the
        // only way they will find out.
        assert!(render("${{ issue.title", &payload()).is_err());
    }
}
