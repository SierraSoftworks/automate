use yew::prelude::*;

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