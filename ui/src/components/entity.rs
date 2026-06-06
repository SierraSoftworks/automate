use yew::prelude::*;

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
    /// Labelled metadata facts shown beneath the key (always visible).
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
    // Render the payload as a pretty-printed string and place it in a text node
    // so any markup it contains is escaped rather than interpreted.
    let pretty =
        serde_json::to_string_pretty(&props.payload).unwrap_or_else(|_| props.payload.to_string());

    let metadata = if props.metadata.is_empty() {
        html! {}
    } else {
        html! {
            <div class="db-entity__meta">
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
                { metadata }
            </summary>
            <pre class="db-entity__payload"><code>{ pretty }</code></pre>
        </details>
    }
}

#[derive(Properties, PartialEq)]
pub struct PartitionProps {
    /// The partition's name.
    pub name: AttrValue,
    /// The number of entities contained within the partition.
    pub count: usize,
    /// The entities ([`DbEntity`]) belonging to this partition.
    #[prop_or_default]
    pub children: Html,
}

/// A collapsible container grouping the entities of a single partition. It is
/// collapsed by default and displays a marker with the number of entities it
/// holds.
#[function_component(Partition)]
pub fn partition(props: &PartitionProps) -> Html {
    html! {
        <details class="partition">
            <summary class="partition__summary">
                <span class="partition__chevron" aria-hidden="true" />
                <span class="partition__name">{ props.name.clone() }</span>
                <span class="partition__count">{ props.count }</span>
            </summary>
            <div class="partition__entries">{ props.children.clone() }</div>
        </details>
    }
}
