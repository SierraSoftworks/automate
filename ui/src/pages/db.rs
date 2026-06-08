use std::collections::BTreeMap;

use automate_api::KeyValueEntry;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{self, ApiError};
use crate::app::AuthHandle;
use crate::components::{
    Alert, AlertKind, BrowserEntry, BrowserPartition, DbEntity, PageActions, PartitionBrowser,
    RefreshButton,
};
use crate::fixtures;

enum Load {
    Loading,
    Ready(Vec<KeyValueEntry>),
    Failed(ApiError),
}

#[function_component(Db)]
pub fn db() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let state = use_state(|| Load::Loading);
    let reload = use_state(|| 0u32);

    {
        let state = state.clone();
        use_effect_with(*reload, move |_| {
            state.set(Load::Loading);
            spawn_local(async move {
                if fixtures::is_demo() {
                    state.set(Load::Ready(fixtures::kv_entries()));
                    return;
                }
                match api::list_kv().await {
                    Ok(entries) => state.set(Load::Ready(entries)),
                    Err(error) => state.set(Load::Failed(error)),
                }
            });
            || ()
        });
    }

    let on_delete = {
        let reload = reload.clone();
        let state = state.clone();
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
            let reload = reload.clone();
            spawn_local(async move {
                let _ = api::delete_kv(&partition, &key).await;
                reload.set(*reload + 1);
            });
        })
    };

    let retry = {
        let reload = reload.clone();
        Callback::from(move |_: MouseEvent| reload.set(*reload + 1))
    };

    // Publish a refresh button into the page title row that re-fetches the store
    // in place. It is cleared when the page unmounts.
    let page_actions = use_context::<PageActions>();
    {
        let page_actions = page_actions.clone();
        let reload = reload.clone();
        let busy = matches!(&*state, Load::Loading);
        use_effect_with((busy, *reload), move |&(busy, reload_count)| {
            if let Some(actions) = &page_actions {
                let onclick = {
                    let reload = reload.clone();
                    Callback::from(move |_: MouseEvent| reload.set(reload_count + 1))
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
                            let content = html! {
                                <DbEntity
                                    partition={partition_for_entity}
                                    entity_key={key.clone()}
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
