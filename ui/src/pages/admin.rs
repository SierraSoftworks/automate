//! The unified admin browser. It loads both the key-value store and the job
//! queues and presents their partitions in a single browser, distinguished by a
//! store-kind icon. A refresh control re-fetches both stores in place and the
//! view re-renders every second so live timestamps stay current.

use automate_api::{KeyValueEntry, QueueMessage};
use gloo_timers::callback::Interval;
use std::collections::BTreeSet;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{self, ApiError};
use crate::app::AuthHandle;
use crate::components::{
    Alert, AlertKind, BrowserPartition, ConnectMenu, PageActions, PartitionBrowser, RefreshButton,
};
use crate::fixtures;
use crate::search::{SearchVocabulary, VocabularyContext};

use super::{kv, queue};

/// Forces a re-render once per second so live relative times (cache expiry and
/// queue timelines) stay current. The interval is torn down on unmount.
#[hook]
fn use_seconds_tick() {
    let trigger = use_force_update();
    use_effect_with((), move |_| {
        let interval = Interval::new(1_000, move || trigger.force_update());
        move || drop(interval)
    });
}

/// The load state of a single store.
enum Load<T> {
    Loading,
    Ready(T),
    Failed(ApiError),
}

impl<T> Load<T> {
    fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(value) => Some(value),
            _ => None,
        }
    }

    fn error(&self) -> Option<&ApiError> {
        match self {
            Load::Failed(error) => Some(error),
            _ => None,
        }
    }
}

/// Fetches the key-value store, replacing the current state in place. This never
/// flips back to [`Load::Loading`], so refreshing an already-loaded page keeps
/// the browser mounted and the user's selection preserved.
async fn fetch_kv(state: UseStateHandle<Load<Vec<KeyValueEntry>>>) {
    if fixtures::is_demo() {
        state.set(Load::Ready(fixtures::kv_entries()));
        return;
    }
    match api::list_kv().await {
        Ok(entries) => state.set(Load::Ready(entries)),
        Err(error) => state.set(Load::Failed(error)),
    }
}

/// Fetches the job queues, replacing the current state in place.
async fn fetch_queue(state: UseStateHandle<Load<Vec<QueueMessage>>>) {
    if fixtures::is_demo() {
        state.set(Load::Ready(fixtures::queue_messages()));
        return;
    }
    match api::list_queue().await {
        Ok(messages) => state.set(Load::Ready(messages)),
        Err(error) => state.set(Load::Failed(error)),
    }
}

#[function_component(Admin)]
pub fn admin() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let kv_state = use_state(|| Load::Loading);
    let queue_state = use_state(|| Load::Loading);
    // Tracks an in-flight in-place refresh so the toolbar button can spin without
    // tearing the loaded view down.
    let refreshing = use_state(|| false);

    // Re-render every second so live relative times stay current.
    use_seconds_tick();

    // Initial load on mount. This is the only path that leaves either store in
    // the loading state; every subsequent fetch updates the data in place.
    {
        let kv_state = kv_state.clone();
        let queue_state = queue_state.clone();
        use_effect_with((), move |_| {
            spawn_local(fetch_kv(kv_state));
            spawn_local(fetch_queue(queue_state));
            || ()
        });
    }

    // Re-fetches both stores without unmounting the browser, used by the toolbar
    // refresh button and after a mutation.
    let refresh = {
        let kv_state = kv_state.clone();
        let queue_state = queue_state.clone();
        let refreshing = refreshing.clone();
        Callback::from(move |_: ()| {
            let kv_state = kv_state.clone();
            let queue_state = queue_state.clone();
            let refreshing = refreshing.clone();
            refreshing.set(true);
            spawn_local(async move {
                fetch_kv(kv_state).await;
                fetch_queue(queue_state).await;
                refreshing.set(false);
            });
        })
    };

    let on_delete_kv = {
        let kv_state = kv_state.clone();
        let refresh = refresh.clone();
        Callback::from(move |(partition, key): (String, String)| {
            if fixtures::is_demo() {
                if let Load::Ready(entries) = &*kv_state {
                    let remaining = entries
                        .iter()
                        .filter(|entry| !(entry.partition == partition && entry.key == key))
                        .cloned()
                        .collect();
                    kv_state.set(Load::Ready(remaining));
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

    let on_delete_queue = {
        let queue_state = queue_state.clone();
        let refresh = refresh.clone();
        Callback::from(move |(partition, key): (String, String)| {
            if fixtures::is_demo() {
                if let Load::Ready(messages) = &*queue_state {
                    let remaining = messages
                        .iter()
                        .filter(|m| !(m.partition == partition && m.key == key))
                        .cloned()
                        .collect();
                    queue_state.set(Load::Ready(remaining));
                }
                return;
            }
            let refresh = refresh.clone();
            spawn_local(async move {
                let _ = api::delete_queue(&partition, &key).await;
                refresh.emit(());
            });
        })
    };

    let on_trigger_queue = {
        let refresh = refresh.clone();
        Callback::from(
            move |(partition, key, payload): (String, String, serde_json::Value)| {
                if fixtures::is_demo() {
                    return;
                }
                let refresh = refresh.clone();
                spawn_local(async move {
                    let _ = api::trigger_queue(&partition, &key, payload).await;
                    refresh.emit(());
                });
            },
        )
    };

    let retry = {
        let kv_state = kv_state.clone();
        let queue_state = queue_state.clone();
        Callback::from(move |_: MouseEvent| {
            kv_state.set(Load::Loading);
            queue_state.set(Load::Loading);
            spawn_local(fetch_kv(kv_state.clone()));
            spawn_local(fetch_queue(queue_state.clone()));
        })
    };

    // Publish the toolbar actions into the page title row: a Connect dropdown for
    // wiring up integrations, and a refresh button that re-fetches both stores in
    // place. They are cleared when the page unmounts.
    let page_actions = use_context::<PageActions>();
    {
        let page_actions = page_actions.clone();
        let refresh = refresh.clone();
        let loading = matches!(&*kv_state, Load::Loading) || matches!(&*queue_state, Load::Loading);
        let busy = *refreshing || loading;
        use_effect_with(busy, move |&busy| {
            if let Some(actions) = &page_actions {
                let onclick = {
                    let refresh = refresh.clone();
                    Callback::from(move |_: MouseEvent| refresh.emit(()))
                };
                actions.set(html! {
                    <div class="page-title__actions">
                        <ConnectMenu />
                        <RefreshButton {onclick} {busy} />
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

    // Publish the completion vocabulary (partition names, keys, kinds) so the
    // app bar can offer value completions for scoped terms. It is derived from
    // the raw store data — not the built partitions — so this runs before the
    // early returns below and only re-publishes when the underlying values
    // change. Sets are de-duplicated and ordered.
    let vocabulary_ctx = use_context::<VocabularyContext>();
    {
        let mut partitions = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut kinds = BTreeSet::new();
        if let Some(entries) = kv_state.ready() {
            if !entries.is_empty() {
                kinds.insert(AttrValue::from("kv"));
            }
            for entry in entries {
                partitions.insert(AttrValue::from(entry.partition.clone()));
                keys.insert(AttrValue::from(entry.key.clone()));
            }
        }
        if let Some(messages) = queue_state.ready() {
            if !messages.is_empty() {
                kinds.insert(AttrValue::from("queue"));
            }
            for message in messages {
                partitions.insert(AttrValue::from(message.partition.clone()));
                keys.insert(AttrValue::from(message.key.clone()));
            }
        }
        let vocabulary = SearchVocabulary {
            partitions: partitions.into_iter().collect(),
            keys: keys.into_iter().collect(),
            kinds: kinds.into_iter().collect(),
        };
        use_effect_with(vocabulary, move |vocabulary| {
            if let Some(ctx) = &vocabulary_ctx {
                ctx.set.emit(vocabulary.clone());
            }
            || ()
        });
    }

    // Still performing the initial load of one of the stores.
    if matches!(&*kv_state, Load::Loading) || matches!(&*queue_state, Load::Loading) {
        return html! { <p class="loading-note">{ "Loading…" }</p> };
    }

    // A session that expired affects both stores; surface a single sign-in
    // prompt rather than two failure banners.
    let unauthorized = matches!(kv_state.error(), Some(ApiError::Unauthorized))
        || matches!(queue_state.error(), Some(ApiError::Unauthorized));

    if unauthorized {
        let login = auth.login.clone();
        let onclick = Callback::from(move |_: MouseEvent| login.emit(()));
        return html! {
            <Alert
                kind={AlertKind::Error}
                title="Your session has expired"
                message="Sign in again to inspect the admin stores."
            >
                <button class="btn btn--small btn--primary" {onclick}>{ "Sign in" }</button>
            </Alert>
        };
    }

    // Both stores failed for some other reason: show a single error with retry.
    if let (Some(error), Some(_)) = (kv_state.error(), queue_state.error()) {
        return html! {
            <Alert
                kind={AlertKind::Error}
                title="Couldn't load the admin stores"
                message={error.to_string()}
            >
                <button class="btn btn--small" onclick={retry}>{ "Retry" }</button>
            </Alert>
        };
    }

    // Build the combined partition list from whichever stores loaded.
    let mut partitions: Vec<BrowserPartition> = Vec::new();
    if let Some(entries) = kv_state.ready() {
        partitions.extend(kv::kv_partitions(entries, &on_delete_kv));
    }
    if let Some(messages) = queue_state.ready() {
        partitions.extend(queue::queue_partitions(
            messages,
            &on_trigger_queue,
            &on_delete_queue,
        ));
    }
    // Sort partitions by name (then kind) so same-named partitions of different
    // kinds sit adjacent, distinguished by their icons.
    partitions.sort_by(|a, b| a.name.cmp(&b.name).then(a.kind.cmp(&b.kind)));

    // One store failed (but not with an auth error): show a non-blocking banner
    // and still render whatever loaded.
    let banner = match (kv_state.error(), queue_state.error()) {
        (Some(error), None) => Some(("key-value store", error.to_string())),
        (None, Some(error)) => Some(("job queues", error.to_string())),
        _ => None,
    };
    let banner = match banner {
        Some((store, message)) => html! {
            <div class="admin__banner">
                <Alert
                    kind={AlertKind::Warning}
                    title={format!("Couldn't load the {store}")}
                    message={message}
                />
            </div>
        },
        None => html! {},
    };

    html! {
        <>
            { banner }
            <PartitionBrowser
                partitions={partitions}
                empty="No partitions found in the key-value store or job queues."
            />
        </>
    }
}
