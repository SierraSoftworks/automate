//! Builds the queue half of the unified admin browser: the `queue`-kind
//! partitions, their messages rendered with a schedule/availability timeline,
//! and the pipeline icon that marks them in the partition list.

use std::collections::BTreeMap;

use automate_api::{QueueMessage, QueueStatus};
use yew::prelude::*;
use yewi::prelude::*;

use crate::components::{BrowserEntry, BrowserPartition, DbEntity, EntityMetadata};
use crate::util;

/// The store kind reported to the browser for `kind:` filtering.
const KIND: &str = "queue";

/// The icon a queue partition wears in the data browser.
pub const QUEUE_ICON: IconData = icons::LIST;

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
                icon: QUEUE_ICON,
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
            <Button variant={ButtonVariant::Primary} size={Size::Small} on_click={trigger_onclick}>
                { "Trigger" }
            </Button>
            <Button variant={ButtonVariant::Danger} size={Size::Small} on_click={delete_onclick}>
                { "Delete" }
            </Button>
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

/// Builds the right-hand state node of the timeline: an outbox when delayed, an
/// outbox with a notification dot when pending, or a spinning retry glyph when
/// reserved/processing. The node reveals an ISO 8601 popover on hover/focus.
fn state_node(status: QueueStatus, label: String, instant: chrono::DateTime<chrono::Utc>) -> Html {
    let (icon, icon_class, dot) = match status {
        QueueStatus::Reserved => (
            icons::REFRESH_CW,
            "queue-tl__icon queue-tl__icon--spin",
            false,
        ),
        QueueStatus::Pending => (
            icons::UPLOAD,
            "queue-tl__icon queue-tl__icon--pending",
            true,
        ),
        QueueStatus::Delayed => (icons::UPLOAD, "queue-tl__icon", false),
    };
    let dot = if dot {
        html! { <span class="queue-tl__dot" /> }
    } else {
        html! {}
    };
    html! {
        <Tooltip class="queue-tl__node" tip={util::format_iso8601(instant)} focusable=true>
            <span class={icon_class}><Icon {icon} size="15px" />{ dot }</span>
            <span class="queue-tl__label">{ label }</span>
        </Tooltip>
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
                { state_node(msg.status, util::short_relative(queued), queued) }
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
            <Tooltip class="queue-tl__node" tip={util::format_iso8601(queued)} focusable=true>
                <span class="queue-tl__icon"><Icon icon={icons::INBOX} size="15px" /></span>
                <span class="queue-tl__label">{ util::short_relative(queued) }</span>
            </Tooltip>
            <span class="queue-tl__track">
                <span class="queue-tl__duration">{ util::short_duration(total) }</span>
                <span class="queue-tl__fill" style={fill_style} />
                { marker }
            </span>
            { state_node(msg.status, util::short_relative(visible), visible) }
        </div>
    }
}
