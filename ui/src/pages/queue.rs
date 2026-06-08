use std::collections::BTreeMap;

use automate_api::{QueueMessage, QueueStatus};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{self, ApiError};
use crate::app::AuthHandle;
use crate::components::{
    Alert, AlertKind, BrowserEntry, BrowserPartition, DbEntity, EntityMetadata, PartitionBrowser,
};
use crate::fixtures;
use crate::util;

enum Load {
    Loading,
    Ready(Vec<QueueMessage>),
    Failed(ApiError),
}

/// A queue message prepared for display, with timestamps already formatted for
/// the client's locale-independent presentation.
#[derive(Clone, PartialEq)]
struct QueueMessageDisplay {
    partition: String,
    key: String,
    payload: serde_json::Value,
    status: String,
    scheduled_at: chrono::DateTime<chrono::Utc>,
    scheduled_at_abs: String,
    scheduled_at_rel: String,
    hidden_until_abs: Option<String>,
    hidden_until_rel: Option<String>,
    traceparent: Option<String>,
}

fn to_display(msg: &QueueMessage) -> QueueMessageDisplay {
    let status = match msg.status {
        QueueStatus::Pending => "Pending",
        QueueStatus::Reserved => "Reserved",
        QueueStatus::Delayed => "Delayed",
    }
    .to_string();

    QueueMessageDisplay {
        partition: msg.partition.clone(),
        key: msg.key.clone(),
        payload: msg.payload.clone(),
        status,
        scheduled_at: msg.scheduled_at,
        scheduled_at_abs: util::format_abs(msg.scheduled_at),
        scheduled_at_rel: util::relative_time(msg.scheduled_at),
        hidden_until_abs: msg.hidden_until.map(util::format_abs),
        hidden_until_rel: msg.hidden_until.map(util::relative_time),
        traceparent: msg.traceparent.clone(),
    }
}

#[function_component(Queue)]
pub fn queue() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let state = use_state(|| Load::Loading);
    let reload = use_state(|| 0u32);

    {
        let state = state.clone();
        use_effect_with(*reload, move |_| {
            state.set(Load::Loading);
            spawn_local(async move {
                if fixtures::is_demo() {
                    state.set(Load::Ready(fixtures::queue_messages()));
                    return;
                }
                match api::list_queue().await {
                    Ok(messages) => state.set(Load::Ready(messages)),
                    Err(error) => state.set(Load::Failed(error)),
                }
            });
            || ()
        });
    }

    let on_trigger = {
        let reload = reload.clone();
        Callback::from(move |msg: QueueMessageDisplay| {
            if fixtures::is_demo() {
                return;
            }
            let reload = reload.clone();
            spawn_local(async move {
                let _ = api::trigger_queue(&msg.partition, &msg.key, msg.payload.clone()).await;
                reload.set(*reload + 1);
            });
        })
    };

    let on_delete = {
        let reload = reload.clone();
        let state = state.clone();
        Callback::from(move |(partition, key): (String, String)| {
            if fixtures::is_demo() {
                if let Load::Ready(messages) = &*state {
                    let remaining = messages
                        .iter()
                        .filter(|m| !(m.partition == partition && m.key == key))
                        .cloned()
                        .collect();
                    state.set(Load::Ready(remaining));
                }
                return;
            }
            let reload = reload.clone();
            spawn_local(async move {
                let _ = api::delete_queue(&partition, &key).await;
                reload.set(*reload + 1);
            });
        })
    };

    let retry = {
        let reload = reload.clone();
        Callback::from(move |_: MouseEvent| reload.set(*reload + 1))
    };

    match &*state {
        Load::Loading => html! { <p class="loading-note">{ "Loading…" }</p> },
        Load::Failed(error) => {
            let needs_login = matches!(error, ApiError::Unauthorized);
            html! {
                <Alert
                    kind={AlertKind::Error}
                    title="Couldn't load the queue"
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
        Load::Ready(messages) => {
            let mut display: Vec<QueueMessageDisplay> = messages.iter().map(to_display).collect();
            display.sort_by(|a, b| a.scheduled_at.cmp(&b.scheduled_at));

            // Group by partition (alphabetically), preserving the schedule order
            // within each partition.
            let mut groups: BTreeMap<String, Vec<QueueMessageDisplay>> = BTreeMap::new();
            for msg in display {
                groups.entry(msg.partition.clone()).or_default().push(msg);
            }

            let partitions: Vec<BrowserPartition> = groups
                .into_iter()
                .map(|(partition, messages)| {
                    let entries = messages
                        .into_iter()
                        .map(|msg| BrowserEntry {
                            key: msg.key.clone().into(),
                            content: queue_entry(&msg, &on_trigger, &on_delete),
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
                    empty="No messages found in any queue."
                />
            }
        }
    }
}

fn queue_entry(
    msg: &QueueMessageDisplay,
    on_trigger: &Callback<QueueMessageDisplay>,
    on_delete: &Callback<(String, String)>,
) -> Html {
    let status_class = format!("badge badge--{}", msg.status.to_lowercase());

    let mut metadata = vec![EntityMetadata::new(
        "Scheduled",
        format!("{} ({})", msg.scheduled_at_rel, msg.scheduled_at_abs),
    )];
    if let (Some(abs), Some(rel)) = (&msg.hidden_until_abs, &msg.hidden_until_rel) {
        metadata.push(EntityMetadata::new("Available", format!("{rel} ({abs})")));
    }
    if let Some(tp) = &msg.traceparent {
        metadata.push(EntityMetadata::new("Trace", tp.clone()));
    }

    let trigger_onclick = {
        let on_trigger = on_trigger.clone();
        let msg = msg.clone();
        Callback::from(move |_| on_trigger.emit(msg.clone()))
    };
    let delete_onclick = {
        let on_delete = on_delete.clone();
        let partition = msg.partition.clone();
        let key = msg.key.clone();
        Callback::from(move |_| on_delete.emit((partition.clone(), key.clone())))
    };

    let controls = html! {
        <>
            <span class={status_class}>{ &msg.status }</span>
            <button class="btn btn--small btn--primary" onclick={trigger_onclick}>
                { "Trigger" }
            </button>
            <button class="btn btn--small btn--danger" onclick={delete_onclick}>
                { "Delete" }
            </button>
        </>
    };

    html! {
        <DbEntity
            partition={msg.partition.clone()}
            entity_key={msg.key.clone()}
            metadata={metadata}
            payload={msg.payload.clone()}
        >
            { controls }
        </DbEntity>
    }
}
