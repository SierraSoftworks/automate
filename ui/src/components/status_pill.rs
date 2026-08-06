//! A small coloured pill saying how something is doing.
//!
//! Used wherever a row has a health to report — a connection that needs
//! reconnecting, a workflow whose last run failed — so that the same condition
//! looks the same wherever it appears.

use automate_api::AuditOutcome;
use yew::prelude::*;

/// How much attention the pill is asking for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    /// Working as intended.
    Ok,
    /// Working for now, but something needs doing.
    Warning,
    /// Not working.
    Error,
    /// Neither good nor bad: paused, skipped, or not yet tried.
    Neutral,
}

impl StatusTone {
    /// The tone an audited outcome is shown in, kept in one place so that a
    /// failure looks the same in the activity list as it does on the row it
    /// happened to.
    pub fn of_outcome(outcome: AuditOutcome) -> Self {
        match outcome {
            AuditOutcome::Success => StatusTone::Ok,
            AuditOutcome::Failure => StatusTone::Error,
            AuditOutcome::Denied => StatusTone::Warning,
            AuditOutcome::Skipped => StatusTone::Neutral,
        }
    }

    fn class(self) -> &'static str {
        match self {
            StatusTone::Ok => "status-pill--ok",
            StatusTone::Warning => "status-pill--warning",
            StatusTone::Error => "status-pill--error",
            StatusTone::Neutral => "status-pill--neutral",
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct StatusPillProps {
    pub tone: StatusTone,

    pub label: AttrValue,

    /// The longer explanation, shown on hover. A pill has room for two or three
    /// words; the reason something failed rarely fits in that.
    #[prop_or_default]
    pub title: Option<AttrValue>,
}

#[function_component(StatusPill)]
pub fn status_pill(props: &StatusPillProps) -> Html {
    html! {
        <span
            class={classes!("status-pill", props.tone.class())}
            title={props.title.clone()}
        >
            { props.label.clone() }
        </span>
    }
}
