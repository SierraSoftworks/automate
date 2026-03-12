use yew::prelude::*;

// ...existing KeyValueView code...

#[derive(Clone, PartialEq)]
pub struct QueueMessageDisplay {
    pub key: String,
    pub payload: serde_json::Value,
    /// "Pending", "Reserved", or "Delayed"
    pub status: String,
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
    pub partition: String,
    pub messages: Vec<QueueMessageDisplay>,
}

#[function_component(QueueView)]
pub fn queue_view(props: &QueueViewProps) -> Html {
    html! {
        <div class="queue-view">
            <div class="kv-header">
                <span class="kv-partition">{ &props.partition }</span>
                <span class="kv-count">{ format!("{} messages", props.messages.len()) }</span>
            </div>
            {
                if props.messages.is_empty() {
                    html! {
                        <div class="kv-empty">
                            <p>{ "No messages found in this queue." }</p>
                        </div>
                    }
                } else {
                    html! {
                        <div class="queue-entries">
                            { for props.messages.iter().map(|msg| {
                                let status_class = format!("queue-status status-{}", msg.status.to_lowercase());
                                let pretty = serde_json::to_string_pretty(&msg.payload)
                                    .unwrap_or_else(|_| msg.payload.to_string());
                                html! {
                                    <div class="queue-entry">
                                        <div class="queue-entry-head">
                                            <div class="queue-entry-key">{ &msg.key }</div>
                                            <div class={status_class}>{ &msg.status }</div>
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