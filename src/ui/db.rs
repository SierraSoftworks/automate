use yew::prelude::*;

// ...existing KeyValueView code...

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

    html! {
        <div class="queue-view">
            {
                if props.messages.is_empty() {
                    html! {
                        <div class="kv-empty">
                            <p>{ "No messages found in any queue." }</p>
                        </div>
                    }
                } else {
                    html! {
                        <div class="kv-overview">
                            { for groups.into_iter().map(|(partition, messages)| {
                                html! {
                                    <div class="queue-partition">
                                        <div class="kv-header">
                                            <span class="kv-partition">{ partition }</span>
                                            <span class="kv-count">{ format!("{} messages", messages.len()) }</span>
                                        </div>
                                        <div class="queue-entries">
                                            { for messages.into_iter().map(|msg| {
                                                let status_class = format!("queue-status status-{}", msg.status.to_lowercase());
                                                let pretty = serde_json::to_string_pretty(&msg.payload)
                                                    .unwrap_or_else(|_| msg.payload.to_string());
                                                let payload_json = serde_json::to_string(&msg.payload)
                                                    .unwrap_or_default();
                                                html! {
                                                    <div class="queue-entry">
                                                        <div class="queue-entry-head">
                                                            <div class="queue-entry-key">
                                                                { &msg.key }
                                                            </div>
                                                            <div class="queue-entry-actions">
                                                                <div class={status_class}>{ &msg.status }</div>
                                                                <form method="post" action="/admin/queue/trigger">
                                                                    <input type="hidden" name="partition" value={msg.partition.clone()} />
                                                                    <input type="hidden" name="key" value={msg.key.clone()} />
                                                                    <input type="hidden" name="payload" value={payload_json} />
                                                                    <input type="hidden" name="csrf_token" value={props.csrf_token.clone()} />
                                                                    <button
                                                                        class="admin-action-btn queue-trigger-btn"
                                                                        type="submit"
                                                                    >{ "trigger" }</button>
                                                                </form>
                                                                <form method="post" action="/admin/queue/delete">
                                                                    <input type="hidden" name="partition" value={msg.partition.clone()} />
                                                                    <input type="hidden" name="key" value={msg.key.clone()} />
                                                                    <input type="hidden" name="csrf_token" value={props.csrf_token.clone()} />
                                                                    <button
                                                                        class="admin-action-btn queue-delete-btn"
                                                                        type="submit"
                                                                    >{ "delete" }</button>
                                                                </form>
                                                            </div>
                                                        </div>
                                                        <div class="queue-entry-meta">
                                                            <span class="queue-meta-item">
                                                                <span class="queue-meta-label">{ "Scheduled" }</span>
                                                                { format!(" {} ({})", msg.scheduled_at_rel, msg.scheduled_at_abs) }
                                                            </span>
                                                            {
                                                                if let (Some(abs), Some(rel)) = (&msg.hidden_until_abs, &msg.hidden_until_rel) {
                                                                    html! {
                                                                        <span class="queue-meta-item">
                                                                            <span class="queue-meta-label">{ "Available" }</span>
                                                                            { format!(" {} ({})", rel, abs) }
                                                                        </span>
                                                                    }
                                                                } else {
                                                                    html! {}
                                                                }
                                                            }
                                                            {
                                                                if let Some(tp) = &msg.traceparent {
                                                                    html! {
                                                                        <span class="queue-meta-item">
                                                                            <span class="queue-meta-label">{ "Trace" }</span>
                                                                            { format!(" {tp}") }
                                                                        </span>
                                                                    }
                                                                } else {
                                                                    html! {}
                                                                }
                                                            }
                                                        </div>
                                                        <pre class="kv-entry-value"><code>{ pretty }</code></pre>
                                                    </div>
                                                }
                                            }) }
                                        </div>
                                    </div>
                                }
                            }) }
                        </div>
                    }
                }
            }
        </div>
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
        <div class="kv-view">
            <div class="kv-header">
                <span class="kv-partition">{ &props.partition }</span>
                <span class="kv-count">{ format!("{} entries", props.entries.len()) }</span>
            </div>
            {
                if props.entries.is_empty() {
                    html! {
                        <div class="kv-empty">
                            <p>{ "No entries found in this partition." }</p>
                        </div>
                    }
                } else {
                    html! {
                        <div class="kv-entries">
                            { for props.entries.iter().map(|(key, value)| {
                                let pretty = serde_json::to_string_pretty(value)
                                    .unwrap_or_else(|_| value.to_string());
                                html! {
                                    <div class="kv-entry">
                                        <div class="kv-entry-key">{ key }</div>
                                        <pre class="kv-entry-value"><code>{ pretty }</code></pre>
                                        <div class="kv-entry-actions">
                                            <form method="post" action="/admin/db/delete">
                                                <input type="hidden" name="partition" value={props.partition.clone()} />
                                                <input type="hidden" name="key" value={key.clone()} />
                                                <input type="hidden" name="csrf_token" value={props.csrf_token.clone()} />
                                                <button
                                                    class="admin-action-btn kv-delete-btn"
                                                    type="submit"
                                                >{ "delete" }</button>
                                            </form>
                                        </div>
                                    </div>
                                }
                            }) }
                        </div>
                    }
                }
            }
        </div>
    }
}
