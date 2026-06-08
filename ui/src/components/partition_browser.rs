use web_sys::HtmlInputElement;
use yew::prelude::*;

/// A single entry within a partition. The `key` is used for searching and
/// sorting; `content` is the pre-rendered entity (typically a [`super::DbEntity`]).
#[derive(Clone, PartialEq)]
pub struct BrowserEntry {
    pub key: AttrValue,
    pub content: Html,
}

/// A named partition together with its entries, as supplied to the
/// [`PartitionBrowser`].
#[derive(Clone, PartialEq)]
pub struct BrowserPartition {
    pub name: AttrValue,
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

fn input_value(event: &InputEvent) -> String {
    event
        .target_dyn_into::<HtmlInputElement>()
        .map(|input| input.value())
        .unwrap_or_default()
}

/// A master/detail browser for partitioned key-value data. The left rail lists
/// every partition (filterable by name) and the right pane shows the entries of
/// the selected partition, with an in-partition key filter. This keeps large
/// stores navigable: only one partition's contents are rendered at a time, and
/// both lists can be narrowed by typing.
#[function_component(PartitionBrowser)]
pub fn partition_browser(props: &PartitionBrowserProps) -> Html {
    let selected = use_state(|| None::<String>);
    let partition_filter = use_state(String::new);
    let entry_filter = use_state(String::new);

    if props.partitions.is_empty() {
        return html! {
            <div class="browser browser--empty">
                <p>{ props.empty.clone() }</p>
            </div>
        };
    }

    // Partitions matching the sidebar search.
    let needle = partition_filter.to_lowercase();
    let visible: Vec<&BrowserPartition> = props
        .partitions
        .iter()
        .filter(|p| needle.is_empty() || p.name.to_lowercase().contains(&needle))
        .collect();

    // Resolve the active partition: keep the user's selection when it is still
    // present (and visible), otherwise fall back to the first visible one.
    let active: Option<&BrowserPartition> = selected
        .as_ref()
        .and_then(|name| visible.iter().find(|p| p.name.as_str() == name).copied())
        .or_else(|| visible.first().copied());

    let on_partition_input = {
        let partition_filter = partition_filter.clone();
        Callback::from(move |e: InputEvent| partition_filter.set(input_value(&e)))
    };

    let sidebar = html! {
        <aside class="browser__sidebar">
            <div class="browser__search">
                <input
                    type="search"
                    class="browser__search-input"
                    placeholder="Filter partitions…"
                    value={(*partition_filter).clone()}
                    oninput={on_partition_input}
                />
            </div>
            <ul class="browser__list">
                { for visible.iter().map(|partition| {
                    let name = partition.name.clone();
                    let is_active = active.is_some_and(|a| a.name == partition.name);
                    let onclick = {
                        let selected = selected.clone();
                        let entry_filter = entry_filter.clone();
                        let name = name.to_string();
                        Callback::from(move |_| {
                            selected.set(Some(name.clone()));
                            entry_filter.set(String::new());
                        })
                    };
                    let mut class = classes!("browser__item");
                    if is_active {
                        class.push("browser__item--active");
                    }
                    html! {
                        <li>
                            <button class={class} onclick={onclick}>
                                <span class="browser__item-name">{ name }</span>
                                <span class="browser__item-count">{ partition.entries.len() }</span>
                            </button>
                        </li>
                    }
                }) }
                { if visible.is_empty() {
                    html! { <li class="browser__no-match">{ "No partitions match your filter." }</li> }
                } else {
                    html! {}
                } }
            </ul>
        </aside>
    };

    let detail = match active {
        Some(partition) => {
            let entry_needle = entry_filter.to_lowercase();
            let entries: Vec<&BrowserEntry> = partition
                .entries
                .iter()
                .filter(|e| entry_needle.is_empty() || e.key.to_lowercase().contains(&entry_needle))
                .collect();

            let on_entry_input = {
                let entry_filter = entry_filter.clone();
                Callback::from(move |e: InputEvent| entry_filter.set(input_value(&e)))
            };

            let list = if entries.is_empty() {
                html! {
                    <div class="browser__detail-empty">
                        <p>{ "No entries match your filter." }</p>
                    </div>
                }
            } else {
                html! {
                    <div class="browser__entries">
                        { for entries.into_iter().map(|entry| entry.content.clone()) }
                    </div>
                }
            };

            html! {
                <section class="browser__detail">
                    <header class="browser__detail-head">
                        <h2 class="browser__detail-title">{ partition.name.clone() }</h2>
                        <span class="browser__detail-count">
                            { format!("{} entries", partition.entries.len()) }
                        </span>
                        <div class="browser__detail-search">
                            <input
                                type="search"
                                class="browser__search-input"
                                placeholder="Filter keys…"
                                value={(*entry_filter).clone()}
                                oninput={on_entry_input}
                            />
                        </div>
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
