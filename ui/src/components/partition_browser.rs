use yew::prelude::*;

use crate::search::{MatchContext, SearchContext, SearchFilter};

/// A single entry within a partition.
#[derive(Clone, PartialEq)]
pub struct BrowserEntry {
    /// The entry's key, used for `key:` filtering and as the display label.
    pub key: AttrValue,
    /// A pre-lowercased concatenation of every searchable property, used to
    /// evaluate free-text search terms.
    pub search: AttrValue,
    /// The pre-rendered entity (typically a [`super::DbEntity`]).
    pub content: Html,
}

/// A named partition together with its entries, as supplied to the
/// [`PartitionBrowser`].
#[derive(Clone, PartialEq)]
pub struct BrowserPartition {
    /// A stable identity unique across stores (for example `kv:cron`), used to
    /// track the active selection so partitions of different kinds that share a
    /// name remain distinct.
    pub id: AttrValue,
    /// The partition's display name.
    pub name: AttrValue,
    /// The store kind, used for `kind:` filtering (for example `kv` or `queue`).
    pub kind: AttrValue,
    /// An icon distinguishing the partition's store kind in the sidebar.
    pub icon: Html,
    /// The partition's entries, in display order.
    pub entries: Vec<BrowserEntry>,
}

#[derive(Properties, PartialEq)]
pub struct PartitionBrowserProps {
    /// The partitions to browse, in display order.
    pub partitions: Vec<BrowserPartition>,
    /// Message shown when there are no partitions at all.
    #[prop_or_default]
    pub empty: AttrValue,
}

/// Evaluates the filter against a single entry of a partition.
fn entry_matches(filter: &SearchFilter, partition: &BrowserPartition, entry: &BrowserEntry) -> bool {
    filter.matches(&MatchContext {
        partition: &partition.name,
        key: &entry.key,
        kind: &partition.kind,
        text: &entry.search,
    })
}

/// A master/detail browser for partitioned data drawn from multiple stores. The
/// left rail lists every partition (with a store-kind icon) and the right pane
/// shows the entries of the selected partition. Both lists are narrowed by the
/// shared search filter taken from [`SearchContext`], so a single query filters
/// partitions and entries together. Only one partition's contents are rendered
/// at a time, keeping large stores navigable.
#[function_component(PartitionBrowser)]
pub fn partition_browser(props: &PartitionBrowserProps) -> Html {
    let selected = use_state(|| None::<String>);
    let search = use_context::<SearchContext>();
    let filter = search
        .as_ref()
        .map(|s| s.filter.clone())
        .unwrap_or_default();

    if props.partitions.is_empty() {
        return html! {
            <div class="browser browser--empty">
                <p>{ props.empty.clone() }</p>
            </div>
        };
    }

    // Compute, for each partition, the entries matching the active filter. A
    // partition is visible in the sidebar only when it has at least one matching
    // entry (or when there is no filter at all).
    let visible: Vec<(&BrowserPartition, Vec<&BrowserEntry>)> = props
        .partitions
        .iter()
        .filter_map(|partition| {
            if filter.is_empty() {
                let entries = partition.entries.iter().collect();
                Some((partition, entries))
            } else {
                let entries: Vec<&BrowserEntry> = partition
                    .entries
                    .iter()
                    .filter(|entry| entry_matches(&filter, partition, entry))
                    .collect();
                if entries.is_empty() {
                    None
                } else {
                    Some((partition, entries))
                }
            }
        })
        .collect();

    // Resolve the active partition: keep the user's selection when it is still
    // present (and visible), otherwise fall back to the first visible one.
    let active: Option<&(&BrowserPartition, Vec<&BrowserEntry>)> = selected
        .as_ref()
        .and_then(|id| visible.iter().find(|(p, _)| p.id.as_str() == id))
        .or_else(|| visible.first());

    let sidebar = html! {
        <aside class="browser__sidebar">
            <ul class="browser__list">
                { for visible.iter().map(|(partition, matching)| {
                    let id = partition.id.to_string();
                    let is_active = active.is_some_and(|(a, _)| a.id == partition.id);
                    let onclick = {
                        let selected = selected.clone();
                        let id = id.clone();
                        Callback::from(move |_| selected.set(Some(id.clone())))
                    };
                    let mut class = classes!("browser__item");
                    if is_active {
                        class.push("browser__item--active");
                    }
                    html! {
                        <li>
                            <button class={class} onclick={onclick}>
                                <span class="browser__item-icon" title={partition.kind.clone()}>
                                    { partition.icon.clone() }
                                </span>
                                <span class="browser__item-name">{ partition.name.clone() }</span>
                                <span class="browser__item-count">{ matching.len() }</span>
                            </button>
                        </li>
                    }
                }) }
                { if visible.is_empty() {
                    html! { <li class="browser__no-match">{ "No partitions match your search." }</li> }
                } else {
                    html! {}
                } }
            </ul>
        </aside>
    };

    let detail = match active {
        Some((partition, entries)) => {
            let count = if entries.len() == partition.entries.len() {
                format!("{} entries", partition.entries.len())
            } else {
                format!("{} of {} entries", entries.len(), partition.entries.len())
            };

            let list = if entries.is_empty() {
                html! {
                    <div class="browser__detail-empty">
                        <p>{ "No entries match your search." }</p>
                    </div>
                }
            } else {
                html! {
                    <div class="browser__entries">
                        { for entries.iter().map(|entry| entry.content.clone()) }
                    </div>
                }
            };

            html! {
                <section class="browser__detail">
                    <header class="browser__detail-head">
                        <span class="browser__detail-icon" title={partition.kind.clone()}>
                            { partition.icon.clone() }
                        </span>
                        <h2 class="browser__detail-title">{ partition.name.clone() }</h2>
                        <span class="browser__detail-count">{ count }</span>
                    </header>
                    { list }
                </section>
            }
        }
        None => html! {
            <section class="browser__detail">
                <div class="browser__detail-empty">
                    <p>{ "Select a partition to view its contents." }</p>
                </div>
            </section>
        },
    };

    html! {
        <div class="browser">
            { sidebar }
            { detail }
        </div>
    }
}
