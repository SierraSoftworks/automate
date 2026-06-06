use std::collections::BTreeMap;

use automate_api::KeyValueEntry;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::app::AuthHandle;
use crate::components::{KeyValueView, PageHeader};
use crate::fixtures;

use super::Protected;

enum Load {
    Loading,
    Ready(Vec<KeyValueEntry>),
    Failed(String),
}

#[function_component(Db)]
pub fn db() -> Html {
    html! { <Protected><DbContent /></Protected> }
}

#[function_component(DbContent)]
fn db_content() -> Html {
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
                    Err(error) => state.set(Load::Failed(error.to_string())),
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

    let on_signout = {
        let signout = auth.signout.clone();
        Callback::from(move |_: MouseEvent| signout.emit(()))
    };

    let body = match &*state {
        Load::Loading => html! { <p class="admin-intro">{ "Loading…" }</p> },
        Load::Failed(error) => html! {
            <div class="error-banner">{ error.clone() }</div>
        },
        Load::Ready(entries) if entries.is_empty() => html! {
            <div class="kv-empty"><p>{ "No entries found in the key-value store." }</p></div>
        },
        Load::Ready(entries) => {
            let mut groups: BTreeMap<String, Vec<(String, serde_json::Value)>> = BTreeMap::new();
            for entry in entries {
                groups
                    .entry(entry.partition.clone())
                    .or_default()
                    .push((entry.key.clone(), entry.payload.clone()));
            }
            html! {
                <div class="kv-overview">
                    { for groups.into_iter().map(|(partition, mut entries)| {
                        entries.sort_by(|a, b| a.0.cmp(&b.0));
                        html! {
                            <KeyValueView
                                partition={partition}
                                entries={entries}
                                on_delete={on_delete.clone()}
                            />
                        }
                    }) }
                </div>
            }
        }
    };

    html! {
        <div class="admin-content">
            <PageHeader
                title="Key-Value Store"
                user_name={auth.user.as_ref().map(|u| AttrValue::from(u.name.clone()))}
                user_email={auth.user.as_ref().and_then(|u| u.email.clone()).map(AttrValue::from)}
                on_signout={on_signout}
            />
            { body }
        </div>
    }
}
