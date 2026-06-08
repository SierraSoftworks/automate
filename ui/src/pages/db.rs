use std::collections::BTreeMap;

use automate_api::KeyValueEntry;
use gloo_timers::callback::Interval;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{self, ApiError};
use crate::app::AuthHandle;
use crate::components::{
    Alert, AlertKind, BrowserEntry, BrowserPartition, DbEntity, PageActions, PartitionBrowser,
    RefreshButton,
};
use crate::fixtures;
use crate::util;

/// Forces a re-render once per second so the relative expiry times shown on
/// cache entries stay current. The interval is torn down when the component
/// unmounts.
#[hook]
fn use_seconds_tick() {
    let trigger = use_force_update();
    use_effect_with((), move |_| {
        let interval = Interval::new(1_000, move || trigger.force_update());
        move || drop(interval)
    });
}

/// Detects whether a key-value payload is a cache envelope (`{ value, expires_at }`)
/// and, if so, returns its parsed expiry instant. Cache entries are written by
/// the agent's [`Cache`] layer and wrap their real payload alongside an
/// `expires_at` timestamp.
fn cache_expiry(payload: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    let obj = payload.as_object()?;
    if obj.len() != 2 || !obj.contains_key("value") || !obj.contains_key("expires_at") {
        return None;
    }
    let expires_at = obj.get("expires_at")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(expires_at)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// A stopwatch glyph marking a cache entry's expiry time.
fn stopwatch_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <line x1="10" y1="2" x2="14" y2="2" />
            <line x1="12" y1="14" x2="12" y2="9" />
            <circle cx="12" cy="14" r="8" />
        </svg>
    }
}

/// Builds the always-visible expiry indicator shown beneath a cache entry's key:
/// a stopwatch icon alongside the relative expiry time (for example
/// `expires in 6h` or `expired 30m ago`).
fn cache_expiry_meta(expires_at: chrono::DateTime<chrono::Utc>) -> Html {
    let expired = expires_at <= chrono::Utc::now();
    let relative = util::short_relative(expires_at);
    let text = if expired {
        format!("expired {relative}")
    } else {
        format!("expires {relative}")
    };
    let class = classes!(
        "db-entity__expiry",
        expired.then_some("db-entity__expiry--expired"),
    );
    html! {
        <div class={class} title={util::format_iso8601(expires_at)}>
            <span class="db-entity__expiry-icon">{ stopwatch_icon() }</span>
            <span class="db-entity__expiry-label">{ text }</span>
        </div>
    }
}

enum Load {
    Loading,
    Ready(Vec<KeyValueEntry>),
    Failed(ApiError),
}

/// Fetches the key-value store and stores the result, replacing the current page
/// state in place. This never flips the page back to [`Load::Loading`], so when
/// it is used to refresh an already-loaded page the [`PartitionBrowser`] stays
/// mounted and the user's selected partition, filters, and expanded entries are
/// preserved.
async fn fetch_kv(state: UseStateHandle<Load>) {
    if fixtures::is_demo() {
        state.set(Load::Ready(fixtures::kv_entries()));
        return;
    }
    match api::list_kv().await {
        Ok(entries) => state.set(Load::Ready(entries)),
        Err(error) => state.set(Load::Failed(error)),
    }
}

#[function_component(Db)]
pub fn db() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let state = use_state(|| Load::Loading);
    // Tracks an in-flight in-place refresh so the toolbar button can spin without
    // tearing the loaded view down.
    let refreshing = use_state(|| false);

    // Re-render every second so the relative expiry times shown on cache entries
    // stay current.
    use_seconds_tick();

    // Initial load on mount. This is the only path that leaves the page in the
    // loading state; every subsequent fetch updates the data in place.
    {
        let state = state.clone();
        use_effect_with((), move |_| {
            spawn_local(fetch_kv(state));
            || ()
        });
    }

    // Re-fetches the store without unmounting the browser, used by the toolbar
    // refresh button and after a mutation.
    let refresh = {
        let state = state.clone();
        let refreshing = refreshing.clone();
        Callback::from(move |_: ()| {
            let state = state.clone();
            let refreshing = refreshing.clone();
            refreshing.set(true);
            spawn_local(async move {
                fetch_kv(state).await;
                refreshing.set(false);
            });
        })
    };

    let on_delete = {
        let state = state.clone();
        let refresh = refresh.clone();
        Callback::from(move |(partition, key): (String, String)| {
            if fixtures::is_demo() {
                if let Load::Ready(entries) = &*state {
                    let remaining = entries
                        .iter()
                        .filter(|entry| !(entry.partition == partition && entry.key == key))
                        .cloned()
                        .collect();
                    state.set(Load::Ready(remaining));
                }
                return;
            }
            let refresh = refresh.clone();
            spawn_local(async move {
                let _ = api::delete_kv(&partition, &key).await;
                refresh.emit(());
            });
        })
    };

    let retry = {
        let state = state.clone();
        Callback::from(move |_: MouseEvent| {
            let state = state.clone();
            state.set(Load::Loading);
            spawn_local(fetch_kv(state));
        })
    };

    // Publish a refresh button into the page title row that re-fetches the store
    // in place. It is cleared when the page unmounts.
    let page_actions = use_context::<PageActions>();
    {
        let page_actions = page_actions.clone();
        let refresh = refresh.clone();
        let busy = *refreshing || matches!(&*state, Load::Loading);
        use_effect_with(busy, move |&busy| {
            if let Some(actions) = &page_actions {
                let onclick = {
                    let refresh = refresh.clone();
                    Callback::from(move |_: MouseEvent| refresh.emit(()))
                };
                actions.set(html! { <RefreshButton {onclick} {busy} /> });
            }
            move || {
                if let Some(actions) = page_actions {
                    actions.clear();
                }
            }
        });
    }

    match &*state {
        Load::Loading => html! { <p class="loading-note">{ "Loading…" }</p> },
        Load::Failed(error) => {
            let needs_login = matches!(error, ApiError::Unauthorized);
            html! {
                <Alert
                    kind={AlertKind::Error}
                    title="Couldn't load the key-value store"
                    message={error.to_string()}
                >
                    <button class="btn btn--small" onclick={retry}>{ "Retry" }</button>
                    {
                        if needs_login {
                            let login = auth.login.clone();
                            let onclick = Callback::from(move |_: MouseEvent| login.emit(()));
                            html! { <button class="btn btn--small btn--primary" onclick={onclick}>{ "Sign in" }</button> }
                        } else {
                            html! {}
                        }
                    }
                </Alert>
            }
        }
        Load::Ready(entries) => {
            let mut groups: BTreeMap<String, Vec<(String, serde_json::Value)>> = BTreeMap::new();
            for entry in entries {
                groups
                    .entry(entry.partition.clone())
                    .or_default()
                    .push((entry.key.clone(), entry.payload.clone()));
            }

            let partitions: Vec<BrowserPartition> = groups
                .into_iter()
                .map(|(partition, mut entries)| {
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    let entries = entries
                        .into_iter()
                        .map(|(key, value)| {
                            let on_delete = on_delete.clone();
                            let partition_for_entity = partition.clone();
                            let partition_for_cb = partition.clone();
                            let key_for_cb = key.clone();
                            let onclick = Callback::from(move |_| {
                                on_delete.emit((partition_for_cb.clone(), key_for_cb.clone()));
                            });
                            // Cache entries wrap their payload in an `expires_at`
                            // envelope; surface the relative expiry beneath the key.
                            let meta = cache_expiry(&value)
                                .map(cache_expiry_meta)
                                .unwrap_or_default();
                            // Key the entry by partition + key so that native
                            // `<details>` expansion is preserved across in-place
                            // refreshes but reset when the partition changes
                            // (the keys become disjoint, forcing a remount).
                            let entity_id = format!("{partition_for_entity}\u{0}{key}");
                            let content = html! {
                                <DbEntity
                                    key={entity_id}
                                    partition={partition_for_entity}
                                    entity_key={key.clone()}
                                    meta={meta}
                                    payload={value}
                                >
                                    <button class="btn btn--small btn--danger" onclick={onclick}>
                                        { "Delete" }
                                    </button>
                                </DbEntity>
                            };
                            BrowserEntry {
                                key: key.into(),
                                content,
                            }
                        })
                        .collect();
                    BrowserPartition {
                        name: partition.into(),
                        entries,
                    }
                })
                .collect();

            html! {
                <PartitionBrowser
                    partitions={partitions}
                    empty="No entries found in the key-value store."
                />
            }
        }
    }
}
