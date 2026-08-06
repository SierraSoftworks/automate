//! What the agent has been doing.
//!
//! A workflow runs long after the page that created it has been closed, so
//! everything it does happens out of sight. This is where it becomes visible: a
//! workflow that has been quietly failing every night, a delivery that was
//! refused because its address was wrong, or the change somebody made last week
//! that explains both.
//!
//! Entries name their subject by identifier, because that is what the agent
//! recorded and identifiers outlive the things they name. They are shown by name
//! where the thing still exists, which is nearly always, and by identifier
//! where it does not — a record of a deleted workflow is exactly when the
//! identifier is the only answer available.

use std::collections::BTreeSet;
use std::collections::HashMap;

use automate_api::{AuditCategory, AuditOutcome, AuditRecord, ConnectionSummary, Workflow};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    Alert, AlertKind, JsonHighlight, PageActions, RefreshButton, StatusPill, StatusTone,
};
use crate::search::{
    MatchContext, SearchContext, SearchField, SearchVocabulary, VocabularyContext,
};
use crate::util::{format_iso8601, short_relative};

/// What each subject identifier should be called on screen.
type Names = HashMap<String, String>;

fn names(workflows: &[Workflow], connections: &[ConnectionSummary]) -> Names {
    workflows
        .iter()
        .map(|workflow| (workflow.id.to_string(), workflow.name.clone()))
        .chain(
            connections
                .iter()
                .map(|connection| (connection.id.to_string(), connection.name.clone())),
        )
        .collect()
}

#[function_component(Activity)]
pub fn activity() -> Html {
    let entries = use_state(|| None::<Vec<AuditRecord>>);
    let names = use_state(Names::new);
    let error = use_state(|| None::<String>);
    let refreshing = use_state(|| false);
    let reload = use_state(|| 0u32);

    {
        let (entries, names, error) = (entries.clone(), names.clone(), error.clone());
        let refreshing = refreshing.clone();

        use_effect_with(*reload, move |_| {
            spawn_local(async move {
                refreshing.set(true);

                match api::list_audit(None, None).await {
                    Ok(loaded) => {
                        entries.set(Some(loaded));
                        error.set(None);
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }

                // The names entries are labelled with. Failing to load them is
                // not a failure of this page: an entry still reads correctly
                // with its subject shown as an identifier.
                let workflows = api::list_workflows().await.unwrap_or_default();
                let connections = api::list_service_connections().await.unwrap_or_default();
                names.set(self::names(&workflows, &connections));

                refreshing.set(false);
            });
            || ()
        });
    }

    let on_refresh = {
        let reload = reload.clone();
        Callback::from(move |_: MouseEvent| reload.set(*reload + 1))
    };

    let page_actions = use_context::<PageActions>();
    {
        let page_actions = page_actions.clone();
        let on_refresh = on_refresh.clone();
        let busy = *refreshing;
        use_effect_with(busy, move |_| {
            if let Some(actions) = &page_actions {
                actions.set(html! {
                    <div class="page-title__actions">
                        <RefreshButton onclick={on_refresh} {busy} />
                    </div>
                });
            }
            move || {
                if let Some(actions) = page_actions {
                    actions.clear();
                }
            }
        });
    }

    // The values the search bar completes. Subjects are offered by the name they
    // are shown under, since that is what somebody looking at the list can see.
    let vocabulary_ctx = use_context::<VocabularyContext>();
    {
        let vocabulary_ctx = vocabulary_ctx.clone();
        let subjects: Vec<AttrValue> = entries
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry.subject.as_deref())
            .map(|subject| {
                AttrValue::from(
                    names
                        .get(subject)
                        .cloned()
                        .unwrap_or_else(|| subject.into()),
                )
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let vocabulary = SearchVocabulary::new(vec![
            SearchField::new("subject", "Match a workflow or connection", subjects),
            SearchField::new(
                "category",
                "Match an area of the system",
                AuditCategory::ALL
                    .iter()
                    .map(|category| AttrValue::from(category.as_str()))
                    .collect(),
            ),
            SearchField::new(
                "outcome",
                "Match how it turned out",
                AuditOutcome::ALL
                    .iter()
                    .map(|outcome| AttrValue::from(outcome.as_str()))
                    .collect(),
            ),
        ]);

        use_effect_with(vocabulary, move |vocabulary| {
            if let Some(ctx) = &vocabulary_ctx {
                ctx.set.emit(vocabulary.clone());
            }
            || ()
        });
    }

    let search = use_context::<SearchContext>();
    let filter = search
        .as_ref()
        .map(|search| search.filter.clone())
        .unwrap_or_default();

    let body = match &*entries {
        None => html! { <p class="activity__empty">{ "Loading…" }</p> },
        Some(loaded) if loaded.is_empty() => html! {
            <p class="activity__empty">
                { "Nothing has happened yet. Runs, deliveries and changes will be listed here." }
            </p>
        },
        Some(loaded) => {
            let visible: Vec<&AuditRecord> = loaded
                .iter()
                .filter(|entry| {
                    let subject = subject_label(entry, &names);
                    let text = format!(
                        "{} {} {} {} {}",
                        subject,
                        entry.category.as_str(),
                        entry.action,
                        entry.outcome.as_str(),
                        entry.message.as_deref().unwrap_or_default(),
                    )
                    .to_lowercase();

                    filter.matches(&MatchContext {
                        fields: &[
                            ("subject", &subject),
                            ("category", entry.category.as_str()),
                            ("outcome", entry.outcome.as_str()),
                        ],
                        text: &text,
                    })
                })
                .collect();

            if visible.is_empty() {
                html! { <p class="activity__empty">{ "Nothing matches your search." }</p> }
            } else {
                html! {
                    <ul class="activity__list">
                        { for visible.into_iter().map(|entry| html! {
                            <ActivityRow
                                key={entry.id}
                                entry={entry.clone()}
                                subject={subject_label(entry, &names)}
                            />
                        }) }
                    </ul>
                }
            }
        }
    };

    html! {
        <section class="activity">
            if let Some(message) = (*error).clone() {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not load your activity."
                    message={message}
                />
            }

            { body }
        </section>
    }
}

/// What an entry's subject should be called: its name where we still have one,
/// and the identifier the agent recorded where we do not.
fn subject_label(entry: &AuditRecord, names: &Names) -> String {
    match &entry.subject {
        Some(subject) => names
            .get(subject)
            .cloned()
            .unwrap_or_else(|| subject.clone()),
        None => "This installation".to_string(),
    }
}

#[derive(Properties, PartialEq)]
struct ActivityRowProps {
    entry: AuditRecord,
    subject: String,
}

#[function_component(ActivityRow)]
fn activity_row(props: &ActivityRowProps) -> Html {
    let entry = &props.entry;

    html! {
        <li class="activity-entry">
            <div class="activity-entry__summary">
                <span class="activity-entry__when" title={format_iso8601(entry.occurred_at)}>
                    { short_relative(entry.occurred_at) }
                </span>

                <div class="activity-entry__detail">
                    <span class="activity-entry__subject">{ &props.subject }</span>
                    <span class="activity-entry__meta">
                        { entry.category.label() }{ " · " }{ &entry.action }
                        if let Some(actor) = &entry.actor {
                            { " · by " }{ actor }
                        }
                    </span>
                </div>

                <StatusPill
                    tone={StatusTone::of_outcome(entry.outcome)}
                    label={entry.outcome.label()}
                />
            </div>

            if let Some(message) = &entry.message {
                <p class="activity-entry__message">{ message }</p>
            }

            // Structured detail is the exception rather than the rule, and it is
            // long when it is there, so it stays folded away until it is asked for.
            if let Some(detail) = &entry.detail {
                <details class="activity-entry__more">
                    <summary>{ "Detail" }</summary>
                    <JsonHighlight value={detail.clone()} />
                </details>
            }
        </li>
    }
}
