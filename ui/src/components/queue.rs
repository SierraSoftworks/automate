use std::collections::BTreeMap;

use yew::prelude::*;

use super::{DbEntity, EntityMetadata, Partition};

/// A queue message prepared for display, with timestamps already formatted for
/// the client's locale-independent presentation.
#[derive(Clone, PartialEq)]
pub struct QueueMessageDisplay {
    pub partition: String,
    pub key: String,
    pub payload: serde_json::Value,
    /// "Pending", "Reserved", or "Delayed".
    pub status: String,
    /// Sort key for ordering messages by schedule time.
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    pub scheduled_at_abs: String,
    pub scheduled_at_rel: String,
    /// Present when status is Reserved or Delayed.
    pub hidden_until_abs: Option<String>,
    /// Present when status is Reserved or Delayed.
    pub hidden_until_rel: Option<String>,
    pub traceparent: Option<String>,
}

#[derive(Properties, PartialEq)]
pub struct QueueViewProps {
    pub messages: Vec<QueueMessageDisplay>,
    /// Invoked with the message to re-enqueue when its trigger button is pressed.
    pub on_trigger: Callback<QueueMessageDisplay>,
    /// Invoked with `(partition, key)` when a message's delete button is pressed.
    pub on_delete: Callback<(String, String)>,
}

#[function_component(QueueView)]
pub fn queue_view(props: &QueueViewProps) -> Html {
    if props.messages.is_empty() {
        return html! {
            <div class="kv-empty">
                <p>{ "No messages found in any queue." }</p>
            </div>
        };
    }

    // Group messages by partition (sorted alphabetically), preserving the
    // incoming order within each partition (already sorted by schedule time).
    let mut groups: BTreeMap<&str, Vec<&QueueMessageDisplay>> = BTreeMap::new();
    for msg in &props.messages {
        groups.entry(msg.partition.as_str()).or_default().push(msg);
    }

    html! {
        <div class="kv-overview">
            { for groups.into_iter().map(|(partition, messages)| {
                html! {
                    <Partition name={partition.to_string()} count={messages.len()}>
                        { for messages.into_iter().map(|msg| queue_entry(msg, &props.on_trigger, &props.on_delete)) }
                    </Partition>
                }
            }) }
        </div>
    }
}

fn queue_entry(
    msg: &QueueMessageDisplay,
    on_trigger: &Callback<QueueMessageDisplay>,
    on_delete: &Callback<(String, String)>,
) -> Html {
    let status_class = format!("queue-status status-{}", msg.status.to_lowercase());

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
            <div class={status_class}>{ &msg.status }</div>
            <button class="admin-action-btn queue-trigger-btn" onclick={trigger_onclick}>
                { "trigger" }
            </button>
            <button class="admin-action-btn queue-delete-btn" onclick={delete_onclick}>
                { "delete" }
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
