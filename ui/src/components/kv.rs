use yew::prelude::*;

use super::{DbEntity, Partition};

#[derive(Properties, PartialEq)]
pub struct KeyValueViewProps {
    pub partition: String,
    pub entries: Vec<(String, serde_json::Value)>,
    /// Invoked with `(partition, key)` when an entry's delete button is pressed.
    pub on_delete: Callback<(String, String)>,
}

/// Renders a single key-value partition as a collapsible section with one
/// expandable entity per entry.
#[function_component(KeyValueView)]
pub fn key_value_view(props: &KeyValueViewProps) -> Html {
    html! {
        <Partition name={props.partition.clone()} count={props.entries.len()}>
            { for props.entries.iter().map(|(key, value)| {
                let on_delete = props.on_delete.clone();
                let partition = props.partition.clone();
                let key_for_cb = key.clone();
                let onclick = Callback::from(move |_| {
                    on_delete.emit((partition.clone(), key_for_cb.clone()));
                });
                let controls = html! {
                    <button class="admin-action-btn kv-delete-btn" onclick={onclick}>
                        { "delete" }
                    </button>
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
