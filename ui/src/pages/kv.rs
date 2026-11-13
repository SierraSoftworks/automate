//! Builds the key-value half of the unified admin browser: the `kv`-kind
//! partitions, their cache-aware entries, and the database icon that marks them
//! in the partition list.

use std::collections::BTreeMap;

use automate_api::KeyValueEntry;
use yew::prelude::*;
use yewi::prelude::*;

use crate::components::{BrowserEntry, BrowserPartition, DbEntity};
use crate::util;

/// The store kind reported to the browser for `kind:` filtering.
const KIND: &str = "kv";

/// The icon a key-value partition wears in the data browser.
pub const KV_ICON: IconData = icons::DATABASE;

/// Detects whether a key-value payload is a cache envelope (`{ value, expires_at }`)
/// and, if so, returns its parsed expiry instant. Cache entries are written by
/// the agent's `Cache` layer and wrap their real payload alongside an
/// `expires_at` timestamp.
fn cache_expiry(payload: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    let obj = payload.as_object()?;
    if obj.len() != 2 || !obj.contains_key("value") || !obj.contains_key("expires_at") {
        return None;
    }
    let expires_at = obj.get("expires_at")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(expires_at)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Builds the always-visible expiry indicator shown beneath a cache entry's key:
/// a stopwatch icon alongside the relative expiry time (for example
/// `expires in 6h` or `expired 30m ago`).
fn cache_expiry_meta(expires_at: chrono::DateTime<chrono::Utc>) -> Html {
    let expired = expires_at <= chrono::Utc::now();
    let relative = util::short_relative(expires_at);
    let text = if expired {
        format!("expired {relative}")
    } else {
        format!("expires {relative}")
    };
    let class = classes!(
        "db-entity__expiry",
        expired.then_some("db-entity__expiry--expired"),
    );
    html! {
        <div class={class} title={util::format_iso8601(expires_at)}>
            <Icon icon={icons::TIMER} size="13px" />
            <span class="db-entity__expiry-label">{ text }</span>
        </div>
    }
}

/// Groups the key-value entries into [`BrowserPartition`]s of kind `kv`, each
/// entry rendered as a collapsible [`DbEntity`] with a delete control and, for
/// cache entries, a live relative-expiry indicator.
pub fn kv_partitions(
    entries: &[KeyValueEntry],
    on_delete: &Callback<(String, String)>,
) -> Vec<BrowserPartition> {
    let mut groups: BTreeMap<String, Vec<(String, serde_json::Value)>> = BTreeMap::new();
    for entry in entries {
        groups
            .entry(entry.partition.clone())
            .or_default()
            .push((entry.key.clone(), entry.payload.clone()));
    }

    groups
        .into_iter()
        .map(|(partition, mut entries)| {
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let entries = entries
                .into_iter()
                .map(|(key, value)| {
                    let on_delete = on_delete.clone();
                    let partition_for_entity = partition.clone();
                    let partition_for_cb = partition.clone();
                    let key_for_cb = key.clone();
                    let onclick = Callback::from(move |_| {
                        on_delete.emit((partition_for_cb.clone(), key_for_cb.clone()));
                    });
                    // Cache entries wrap their payload in an `expires_at`
                    // envelope; surface the relative expiry beneath the key.
                    let meta = cache_expiry(&value)
                        .map(cache_expiry_meta)
                        .unwrap_or_default();
                    // A pre-lowercased haystack covering every searchable
                    // property of the entry, used by free-text search terms.
                    let search = format!(
                        "{partition} {key} {KIND} {}",
                        serde_json::to_string(&value).unwrap_or_default()
                    )
                    .to_lowercase();
                    // Key the entry by partition + key so that native
                    // `<details>` expansion is preserved across in-place
                    // refreshes but reset when the partition changes (the keys
                    // become disjoint, forcing a remount).
                    let entity_id = format!("{partition_for_entity}\u{0}{key}");
                    let content = html! {
                        <DbEntity
                            key={entity_id}
                            partition={partition_for_entity}
                            entity_key={key.clone()}
                            meta={meta}
                            payload={value}
                        >
                            <Button variant={ButtonVariant::Danger} size={Size::Small} on_click={onclick}>
                                { "Delete" }
                            </Button>
                        </DbEntity>
                    };
                    BrowserEntry {
                        key: key.into(),
                        search: search.into(),
                        content,
                    }
                })
                .collect();
            BrowserPartition {
                id: format!("{KIND}:{partition}").into(),
                name: partition.into(),
                kind: KIND.into(),
                icon: KV_ICON,
                entries,
            }
        })
        .collect()
}
