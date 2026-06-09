//! Builds the queue half of the unified admin browser: the `queue`-kind
//! partitions, their messages rendered with a schedule/availability timeline,
//! and the pipeline icon that marks them in the partition list.

use std::collections::BTreeMap;

use automate_api::{QueueMessage, QueueStatus};
use yew::prelude::*;

use crate::components::{BrowserEntry, BrowserPartition, DbEntity, EntityMetadata};
use crate::util;

/// The store kind reported to the browser for `kind:` filtering.
const KIND: &str = "queue";

/// A pipeline glyph marking queue partitions in the sidebar.
pub fn queue_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor"
            stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <line x1="8" y1="6" x2="21" y2="6" />
            <line x1="8" y1="12" x2="21" y2="12" />
            <line x1="8" y1="18" x2="21" y2="18" />
            <circle cx="3.5" cy="6" r="1" />
            <circle cx="3.5" cy="12" r="1" />
            <circle cx="3.5" cy="18" r="1" />
        </svg>
    }
}

/// A queue message prepared for display. Timestamps are retained as raw instants
/// so the timeline can both compute positions and render exact ISO 8601 values
/// in its popovers.
#[derive(Clone, PartialEq)]
struct QueueMessageDisplay {
    partition: String,
    key: String,
    payload: serde_json::Value,
    status: QueueStatus,
    /// When the message was originally enqueued.
    scheduled_at: chrono::DateTime<chrono::Utc>,
    /// When the message becomes visible/executable again (delayed/reserved only).
    hidden_until: Option<chrono::DateTime<chrono::Utc>>,
    traceparent: Option<String>,
}

fn to_display(msg: &QueueMessage) -> QueueMessageDisplay {
    QueueMessageDisplay {
        partition: msg.partition.clone(),
        key: msg.key.clone(),
        payload: msg.payload.clone(),
        status: msg.status,
        scheduled_at: msg.scheduled_at,
        hidden_until: msg.hidden_until,
        traceparent: msg.traceparent.clone(),
    }
}

/// Groups the queued messages into [`BrowserPartition`]s of kind `queue`,
/// ordering messages by their idempotency key within each partition. Each
/// message is rendered with a schedule/availability/state timeline plus trigger
/// and delete controls.
pub fn queue_partitions(
    messages: &[QueueMessage],
    on_trigger: &Callback<(String, String, serde_json::Value)>,
    on_delete: &Callback<(String, String)>,
) -> Vec<BrowserPartition> {
    let mut display: Vec<QueueMessageDisplay> = messages.iter().map(to_display).collect();
    display.sort_by(|a, b| a.key.cmp(&b.key));

    // Group by partition (alphabetically), preserving the idempotency-key order
    // within each partition.
    let mut groups: BTreeMap<String, Vec<QueueMessageDisplay>> = BTreeMap::new();
    for msg in display {
        groups.entry(msg.partition.clone()).or_default().push(msg);
    }

    groups
        .into_iter()
        .map(|(partition, messages)| {
            let entries = messages
                .into_iter()
                .map(|msg| {
                    let search = format!(
                        "{} {} {KIND} {:?} {} {}",
                        msg.partition,
                        msg.key,
                        msg.status,
                        msg.traceparent.clone().unwrap_or_default(),
                        serde_json::to_string(&msg.payload).unwrap_or_default()
                    )
                    .to_lowercase();
                    BrowserEntry {
                        key: msg.key.clone().into(),
                        search: search.into(),
                        content: queue_entry(&msg, on_trigger, on_delete),
                    }
                })
                .collect();
            BrowserPartition {
                id: format!("{KIND}:{partition}").into(),
                name: partition.into(),
                kind: KIND.into(),
                icon: queue_icon(),
                entries,
            }
        })
        .collect()
}

fn queue_entry(
    msg: &QueueMessageDisplay,
    on_trigger: &Callback<(String, String, serde_json::Value)>,
    on_delete: &Callback<(String, String)>,
) -> Html {
    // The schedule/availability/state is conveyed by the timeline; the trace
    // parent is revealed alongside the payload when the entry is expanded.
    let trace_meta = msg
        .traceparent
        .as_ref()
        .map(|tp| vec![EntityMetadata::new("Trace", tp.clone())])
        .unwrap_or_default();

    let trigger_onclick = {
        let on_trigger = on_trigger.clone();
        let partition = msg.partition.clone();
        let key = msg.key.clone();
        let payload = msg.payload.clone();
        Callback::from(move |_| on_trigger.emit((partition.clone(), key.clone(), payload.clone())))
    };
    let delete_onclick = {
        let on_delete = on_delete.clone();
        let partition = msg.partition.clone();
        let key = msg.key.clone();
        Callback::from(move |_| on_delete.emit((partition.clone(), key.clone())))
    };

    let controls = html! {
        <>
            <button class="btn btn--small btn--primary" onclick={trigger_onclick}>
                { "Trigger" }
            </button>
            <button class="btn btn--small btn--danger" onclick={delete_onclick}>
                { "Delete" }
            </button>
        </>
    };

    html! {
        <DbEntity
            key={format!("{}\u{0}{}", msg.partition, msg.key)}
            partition={msg.partition.clone()}
            entity_key={msg.key.clone()}
            meta={queue_timeline(msg)}
            metadata={trace_meta}
            payload={msg.payload.clone()}
        >
            { controls }
        </DbEntity>
    }
}

/// An inbox glyph marking the point at which a message was enqueued.
fn inbox_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polyline points="22 12 16 12 14 15 10 15 8 12 2 12" />
            <path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z" />
        </svg>
    }
}

/// An outbox (upload tray) glyph marking the point at which a message becomes
/// available for execution.
fn outbox_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
            <polyline points="17 8 12 3 7 8" />
            <line x1="12" y1="3" x2="12" y2="15" />
        </svg>
    }
}

/// A circular-arrow glyph indicating a message that is currently being
/// processed; spun via CSS.
fn retry_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polyline points="23 4 23 10 17 10" />
            <polyline points="1 20 1 14 7 14" />
            <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
        </svg>
    }
}

/// Which edge a timeline popover is anchored to. Popovers grow toward the
/// centre of the timeline so they never extend past the entry box and get
/// clipped: the left-hand (queued) node anchors to its start, the right-hand
/// (state) node to its end.
#[derive(Clone, Copy)]
enum PopoverAlign {
    Start,
    End,
}

/// A hover/focus popover revealing the exact ISO 8601 datetime for a timeline
/// timestamp.
fn time_popover(instant: chrono::DateTime<chrono::Utc>, align: PopoverAlign) -> Html {
    let class = match align {
        PopoverAlign::Start => "queue-tl__popover queue-tl__popover--start",
        PopoverAlign::End => "queue-tl__popover queue-tl__popover--end",
    };
    html! {
        <span class={class} role="tooltip">
            { util::format_iso8601(instant) }
        </span>
    }
}

/// Builds the right-hand state node of the timeline: an outbox when delayed, an
/// outbox with a notification dot when pending, or a spinning retry glyph when
/// reserved/processing. The node reveals an ISO 8601 popover on hover/focus.
fn state_node(
    status: QueueStatus,
    label: String,
    instant: chrono::DateTime<chrono::Utc>,
    align: PopoverAlign,
) -> Html {
    let (icon, icon_class, dot) = match status {
        QueueStatus::Reserved => (retry_icon(), "queue-tl__icon queue-tl__icon--spin", false),
        QueueStatus::Pending => (
            outbox_icon(),
            "queue-tl__icon queue-tl__icon--pending",
            true,
        ),
        QueueStatus::Delayed => (outbox_icon(), "queue-tl__icon", false),
    };
    let dot = if dot {
        html! { <span class="queue-tl__dot" /> }
    } else {
        html! {}
    };
    html! {
        <span class="queue-tl__node" tabindex="0">
            <span class={icon_class}>{ icon }{ dot }</span>
            <span class="queue-tl__label">{ label }</span>
            { time_popover(instant, align) }
        </span>
    }
}

/// Renders the compact schedule/availability/state timeline shown beneath a
/// queue entry's key.
///
/// When a message has a meaningful hidden span (its enqueue and availability
/// times differ by more than a second) a number-line is drawn between an inbox
/// node (when it was queued) and a state node (when it becomes available),
/// labelled with the total hidden duration. A vertical "now" marker rides the
/// line, advancing to the right as real time passes via a pure-CSS animation —
/// except for reserved messages, which are actively processing.
///
/// When there is no hidden span, only the relevant state node is shown.
fn queue_timeline(msg: &QueueMessageDisplay) -> Html {
    let now = chrono::Utc::now();
    let queued = msg.scheduled_at;

    // The hidden span runs from the enqueue time to the availability time. With
    // no availability time (a plain pending message) there is no span.
    let span_secs = msg
        .hidden_until
        .map(|v| (v - queued).num_seconds().abs())
        .unwrap_or(0);

    if span_secs <= 1 {
        // Collapsed: show just the state node, labelled with how long the
        // message has been waiting.
        return html! {
            <div class="queue-tl queue-tl--collapsed">
                { state_node(msg.status, util::short_relative(queued), queued, PopoverAlign::Start) }
            </div>
        };
    }

    let visible = msg
        .hidden_until
        .expect("span_secs > 1 implies hidden_until is set");
    let total = (visible - queued).num_seconds().max(1);
    // A fractional elapsed keeps the animation seeded at the exact current
    // position, so the per-second re-render re-seeds it seamlessly regardless of
    // when within the second the tick fires.
    let elapsed = ((now - queued).num_milliseconds() as f64 / 1000.0).clamp(0.0, total as f64);
    let pct = (elapsed / total as f64) * 100.0;

    // The "now" marker and elapsed fill advance from the queued point (0%) to
    // the availability point (100%). A negative animation delay seeds them at
    // the current offset so they begin from the right place and continue to the
    // end in real time. Reserved messages are paused (no marker, static fill).
    let (fill_style, marker) = if msg.status == QueueStatus::Reserved {
        (format!("width:{pct:.3}%"), html! {})
    } else {
        let fill = format!(
            "animation:queue-tl-fill {total}s linear forwards;animation-delay:-{elapsed:.3}s"
        );
        let marker_style = format!(
            "animation:queue-tl-now {total}s linear forwards;animation-delay:-{elapsed:.3}s"
        );
        (
            fill,
            html! { <span class="queue-tl__now" style={marker_style} /> },
        )
    };

    html! {
        <div class="queue-tl">
            <span class="queue-tl__node" tabindex="0">
                <span class="queue-tl__icon">{ inbox_icon() }</span>
                <span class="queue-tl__label">{ util::short_relative(queued) }</span>
                { time_popover(queued, PopoverAlign::Start) }
            </span>
            <span class="queue-tl__track">
                <span class="queue-tl__duration">{ util::short_duration(total) }</span>
                <span class="queue-tl__fill" style={fill_style} />
                { marker }
            </span>
            { state_node(msg.status, util::short_relative(visible), visible, PopoverAlign::End) }
        </div>
    }
}
