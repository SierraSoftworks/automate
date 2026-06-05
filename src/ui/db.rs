use yew::prelude::*;

use super::{CsrfToken, DbEntity, EntityMetadata, Partition};

#[derive(Clone, PartialEq)]
pub struct QueueMessageDisplay {
    pub partition: String,
    pub key: String,
    pub payload: serde_json::Value,
    /// "Pending", "Reserved", or "Delayed"
    pub status: String,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    pub scheduled_at_abs: String,
    pub scheduled_at_rel: String,
    /// Present when status is Reserved or Delayed
    pub hidden_until_abs: Option<String>,
    /// Present when status is Reserved or Delayed
    pub hidden_until_rel: Option<String>,
    pub traceparent: Option<String>,
}

#[derive(Properties, PartialEq)]
pub struct QueueViewProps {
    pub messages: Vec<QueueMessageDisplay>,
    pub csrf_token: String,
}

#[function_component(QueueView)]
pub fn queue_view(props: &QueueViewProps) -> Html {
    // Group messages by partition (sorted alphabetically), preserving the
    // incoming order within each partition (already sorted by schedule time).
    let mut groups: std::collections::BTreeMap<&str, Vec<&QueueMessageDisplay>> =
        std::collections::BTreeMap::new();
    for msg in &props.messages {
        groups.entry(msg.partition.as_str()).or_default().push(msg);
    }

    if props.messages.is_empty() {
        return html! {
            <div class="kv-empty">
                <p>{ "No messages found in any queue." }</p>
            </div>
        };
    }

    html! {
        <div class="kv-overview">
            { for groups.into_iter().map(|(partition, messages)| {
                html! {
                    <Partition name={partition.to_string()} count={messages.len()}>
                        { for messages.into_iter().map(|msg| queue_entry(msg, &props.csrf_token)) }
                    </Partition>
                }
            }) }
        </div>
    }
}

fn queue_entry(msg: &QueueMessageDisplay, csrf_token: &str) -> Html {
    let status_class = format!("queue-status status-{}", msg.status.to_lowercase());
    let payload_json = serde_json::to_string(&msg.payload).unwrap_or_default();

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

    let controls = html! {
        <>
            <div class={status_class}>{ &msg.status }</div>
            <form method="post" action="/admin/queue/trigger">
                <input type="hidden" name="partition" value={msg.partition.clone()} />
                <input type="hidden" name="key" value={msg.key.clone()} />
                <input type="hidden" name="payload" value={payload_json} />
                <CsrfToken token={csrf_token.to_string()} />
                <button class="admin-action-btn queue-trigger-btn" type="submit">{ "trigger" }</button>
            </form>
            <form method="post" action="/admin/queue/delete">
                <input type="hidden" name="partition" value={msg.partition.clone()} />
                <input type="hidden" name="key" value={msg.key.clone()} />
                <CsrfToken token={csrf_token.to_string()} />
                <button class="admin-action-btn queue-delete-btn" type="submit">{ "delete" }</button>
            </form>
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

#[derive(Properties, PartialEq)]
pub struct KeyValueViewProps {
    pub partition: String,
    pub entries: Vec<(String, serde_json::Value)>,
    pub csrf_token: String,
}

#[function_component(KeyValueView)]
pub fn key_value_view(props: &KeyValueViewProps) -> Html {
    html! {
        <Partition name={props.partition.clone()} count={props.entries.len()}>
            { for props.entries.iter().map(|(key, value)| {
                let controls = html! {
                    <form method="post" action="/admin/db/delete">
                        <input type="hidden" name="partition" value={props.partition.clone()} />
                        <input type="hidden" name="key" value={key.clone()} />
                        <CsrfToken token={props.csrf_token.clone()} />
                        <button class="admin-action-btn kv-delete-btn" type="submit">{ "delete" }</button>
                    </form>
                };
                html! {
                    <DbEntity
                        partition={props.partition.clone()}
                        entity_key={key.clone()}
                        payload={value.clone()}
                    >
                        { controls }
                    </DbEntity>
                }
            }) }
        </Partition>
    }
}
