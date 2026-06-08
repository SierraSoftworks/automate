use yew::prelude::*;

use super::JsonHighlight;

/// A single labelled metadata fact rendered alongside a [`DbEntity`], such as a
/// scheduled time or a trace parent.
#[derive(Clone, PartialEq)]
pub struct EntityMetadata {
    pub label: AttrValue,
    pub value: AttrValue,
}

impl EntityMetadata {
    pub fn new(label: impl Into<AttrValue>, value: impl Into<AttrValue>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct DbEntityProps {
    /// The partition the entity belongs to.
    pub partition: AttrValue,
    /// The entity's key within its partition.
    pub entity_key: AttrValue,
    /// Custom metadata content rendered beneath the key (for example a queue
    /// timeline), always visible.
    #[prop_or_default]
    pub meta: Html,
    /// Labelled metadata facts revealed alongside the payload when the entity is
    /// expanded (for example a trace parent).
    #[prop_or_default]
    pub metadata: Vec<EntityMetadata>,
    /// The entity's payload, pretty-printed and revealed only when expanded.
    pub payload: serde_json::Value,
    /// Controls (status badges, trigger/delete buttons, ...) rendered in the
    /// entity header.
    #[prop_or_default]
    pub children: Html,
}

/// A collapsible visualization of a single stored entity. The key, metadata and
/// controls are always visible; the payload is hidden until the entity is
/// expanded.
#[function_component(DbEntity)]
pub fn db_entity(props: &DbEntityProps) -> Html {
    let metadata = if props.metadata.is_empty() {
        html! {}
    } else {
        html! {
            <div class="db-entity__meta db-entity__meta--expanded">
                { for props.metadata.iter().map(|item| html! {
                    <span class="db-entity__meta-item">
                        <span class="db-entity__meta-label">{ item.label.clone() }</span>
                        { item.value.clone() }
                    </span>
                }) }
            </div>
        }
    };

    html! {
        <details class="db-entity">
            <summary class="db-entity__summary">
                <div class="db-entity__head">
                    <span class="db-entity__chevron" aria-hidden="true" />
                    <span class="db-entity__key">{ props.entity_key.clone() }</span>
                    <div class="db-entity__controls">{ props.children.clone() }</div>
                </div>
                { props.meta.clone() }
            </summary>
            { metadata }
            <JsonHighlight value={props.payload.clone()} />
        </details>
    }
}
