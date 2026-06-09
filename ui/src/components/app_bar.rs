use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::app::{AuthHandle, AuthStatus};
use crate::search::{FIELD_PREFIXES, SearchContext, VocabularyContext};
use crate::util;

/// Derives up to two uppercase initials from a display name or email address.
fn initials(name: &str) -> String {
    let from_words: String = name
        .split(|c: char| c.is_whitespace() || c == '.' || c == '@' || c == '_' || c == '-')
        .filter(|w| !w.is_empty())
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect();

    let initials = if from_words.is_empty() {
        name.chars().take(2).collect()
    } else {
        from_words
    };

    initials.to_uppercase()
}

fn input_value(event: &InputEvent) -> String {
    event
        .target_dyn_into::<HtmlInputElement>()
        .map(|input| input.value())
        .unwrap_or_default()
}

/// The maximum number of value completions shown at once; the user narrows the
/// list by typing rather than scrolling a huge dropdown.
const MAX_SUGGESTIONS: usize = 8;

/// A single autocomplete entry shown beneath the search input. It is either a
/// `field:` prefix or a concrete value for the field currently being typed.
struct Suggestion {
    /// The primary token shown (a `field:` prefix or a concrete value).
    label: AttrValue,
    /// An optional secondary description (shown for field prefixes).
    desc: Option<AttrValue>,
    /// The full query string this suggestion produces when applied.
    replacement: String,
}

/// A magnifying-glass glyph shown inside the search field.
fn search_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="11" cy="11" r="8" />
            <line x1="21" y1="21" x2="16.65" y2="16.65" />
        </svg>
    }
}

/// The persistent top-level application bar shown across every admin view. It
/// hosts the brand mark, the unified search field, and the signed-in user chip,
/// and stays consistent as the user moves between pages.
#[function_component(AppBar)]
pub fn app_bar() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let search = use_context::<SearchContext>();
    let vocabulary = use_context::<VocabularyContext>();

    let signed_in = matches!(auth.status, AuthStatus::SignedIn(_) | AuthStatus::Disabled);

    // Tracks the highlighted suggestion (navigated with arrow keys) and whether
    // the user has dismissed the dropdown for the current token (via Escape).
    let highlight = use_state(|| None::<usize>);
    let dismissed = use_state(|| false);

    // The unified search field filters both the partition list and entries.
    // `partition:` and `key:` scope a term to a property; a bare term matches
    // every property. The dropdown is context-aware: typing a partial field
    // name suggests matching `field:` prefixes, and once a field is scoped
    // (`partition:`) it suggests the concrete values for that field.
    let search = match (signed_in, search) {
        (true, Some(search)) => {
            let query_str = search.query.to_string();

            // The token currently being typed is the trailing run of
            // non-whitespace characters; everything before it is preserved when
            // a suggestion is applied.
            let active_token = query_str
                .rsplit(char::is_whitespace)
                .next()
                .unwrap_or_default();
            let head = query_str[..query_str.len() - active_token.len()].to_string();

            let suggestions: Vec<Suggestion> = if active_token.is_empty() {
                Vec::new()
            } else if let Some((field, partial)) = active_token.split_once(':') {
                // Value completion: the token is scoped to a field, so suggest
                // the field's concrete values that contain the partial value.
                // Completing a value finishes the term, so a trailing space is
                // appended to ready the input for the next term.
                let needle = partial.to_lowercase();
                vocabulary
                    .as_ref()
                    .and_then(|v| v.vocabulary.values_for(field))
                    .map(|values| {
                        values
                            .iter()
                            .filter(|value| value.to_lowercase().contains(&needle))
                            .take(MAX_SUGGESTIONS)
                            .map(|value| Suggestion {
                                label: value.clone(),
                                desc: None,
                                replacement: format!("{head}{field}:{value} "),
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                // Prefix completion: suggest the `field:` prefixes that start
                // with the partial token. A prefix isn't a complete term, so no
                // trailing space — applying it re-triggers value completion.
                let needle = active_token.to_lowercase();
                FIELD_PREFIXES
                    .iter()
                    .filter(|(prefix, _)| prefix.starts_with(needle.as_str()))
                    .map(|(prefix, desc)| Suggestion {
                        label: AttrValue::from(*prefix),
                        desc: Some(AttrValue::from(*desc)),
                        replacement: format!("{head}{prefix}"),
                    })
                    .collect()
            };

            let show_suggestions = !suggestions.is_empty() && !*dismissed;
            let replacements: Vec<String> =
                suggestions.iter().map(|s| s.replacement.clone()).collect();

            let oninput = {
                let set = search.set.clone();
                let highlight = highlight.clone();
                let dismissed = dismissed.clone();
                Callback::from(move |e: InputEvent| {
                    highlight.set(None);
                    dismissed.set(false);
                    set.emit(input_value(&e));
                })
            };

            let onkeydown = {
                let set = search.set.clone();
                let highlight = highlight.clone();
                let dismissed = dismissed.clone();
                let replacements = replacements.clone();
                Callback::from(move |e: KeyboardEvent| {
                    if replacements.is_empty() {
                        return;
                    }
                    match e.key().as_str() {
                        "ArrowDown" => {
                            e.prevent_default();
                            let next = match *highlight {
                                Some(i) => (i + 1) % replacements.len(),
                                None => 0,
                            };
                            highlight.set(Some(next));
                        }
                        "ArrowUp" => {
                            e.prevent_default();
                            let next = match *highlight {
                                Some(0) | None => replacements.len() - 1,
                                Some(i) => i - 1,
                            };
                            highlight.set(Some(next));
                        }
                        "Enter" | "Tab" => {
                            if let Some(i) = *highlight {
                                e.prevent_default();
                                highlight.set(None);
                                set.emit(replacements[i].clone());
                            }
                        }
                        "Escape" => {
                            e.prevent_default();
                            dismissed.set(true);
                            highlight.set(None);
                        }
                        _ => {}
                    }
                })
            };

            let dropdown = if show_suggestions {
                let items = suggestions
                    .iter()
                    .enumerate()
                    .map(|(i, suggestion)| {
                        let active = *highlight == Some(i);
                        let onmousedown = {
                            let set = search.set.clone();
                            let highlight = highlight.clone();
                            let replacement = replacements[i].clone();
                            Callback::from(move |e: MouseEvent| {
                                // Prevent the input from losing focus on click.
                                e.prevent_default();
                                highlight.set(None);
                                set.emit(replacement.clone());
                            })
                        };
                        let onmouseenter = {
                            let highlight = highlight.clone();
                            Callback::from(move |_: MouseEvent| highlight.set(Some(i)))
                        };
                        let class = classes!(
                            "app-bar__suggestion",
                            active.then_some("app-bar__suggestion--active")
                        );
                        let desc = match &suggestion.desc {
                            Some(desc) => html! {
                                <span class="app-bar__suggestion-desc">{ desc.clone() }</span>
                            },
                            None => html! {},
                        };
                        html! {
                            <li
                                class={class}
                                role="option"
                                aria-selected={active.to_string()}
                                onmousedown={onmousedown}
                                onmouseenter={onmouseenter}
                            >
                                <span class="app-bar__suggestion-token">{ suggestion.label.clone() }</span>
                                { desc }
                            </li>
                        }
                    })
                    .collect::<Html>();
                html! {
                    <ul class="app-bar__suggestions" role="listbox">
                        { items }
                    </ul>
                }
            } else {
                html! {}
            };

            html! {
                <div class="app-bar__search">
                    <span class="app-bar__search-icon">{ search_icon() }</span>
                    <input
                        type="search"
                        class="app-bar__search-input"
                        placeholder="Search… (try partition:cron or key:ynab)"
                        value={search.query.clone()}
                        oninput={oninput}
                        onkeydown={onkeydown}
                        role="combobox"
                        aria-expanded={show_suggestions.to_string()}
                        aria-autocomplete="list"
                    />
                    { dropdown }
                </div>
            }
        }
        _ => html! { <div class="app-bar__spacer" /> },
    };

    let user = match &auth.user {
        Some(user) => {
            let on_signout = {
                let signout = auth.signout.clone();
                Callback::from(move |_: MouseEvent| signout.emit(()))
            };
            let email = match &user.email {
                Some(email) => html! { <span class="user-chip__email">{ email.clone() }</span> },
                None => html! {},
            };
            html! {
                <div class="user-chip">
                    <span class="user-chip__avatar">{ initials(&user.name) }</span>
                    <span class="user-chip__meta">
                        <span class="user-chip__name">{ user.name.clone() }</span>
                        { email }
                    </span>
                    <button class="user-chip__signout" onclick={on_signout}>{ "Sign out" }</button>
                </div>
            }
        }
        None => html! {},
    };

    html! {
        <header class="app-bar">
            <div class="app-bar__inner">
                <a class="app-bar__brand" href={util::nav_href("/admin")}>
                    <img
                        src="https://cdn.sierrasoftworks.com/logos/icon.svg"
                        alt="The Sierra Softworks logo."
                    />
                    <span class="app-bar__brand-name">{ "Automate" }</span>
                </a>
                { search }
                { user }
            </div>
        </header>
    }
}
