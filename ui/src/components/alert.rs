use yew::prelude::*;

/// The severity of an [`Alert`], which selects its colour and icon.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Warning/Info/Success are part of the component's API.
pub enum AlertKind {
    Error,
    Warning,
    Info,
    Success,
}

impl AlertKind {
    fn class(self) -> &'static str {
        match self {
            AlertKind::Error => "alert--error",
            AlertKind::Warning => "alert--warning",
            AlertKind::Info => "alert--info",
            AlertKind::Success => "alert--success",
        }
    }

    /// A simple inline SVG glyph appropriate to the severity.
    fn icon(self) -> Html {
        let path = match self {
            AlertKind::Error => html! {
                <>
                    <circle cx="12" cy="12" r="10" />
                    <line x1="12" y1="8" x2="12" y2="12" />
                    <line x1="12" y1="16" x2="12.01" y2="16" />
                </>
            },
            AlertKind::Warning => html! {
                <>
                    <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
                    <line x1="12" y1="9" x2="12" y2="13" />
                    <line x1="12" y1="17" x2="12.01" y2="17" />
                </>
            },
            AlertKind::Info => html! {
                <>
                    <circle cx="12" cy="12" r="10" />
                    <line x1="12" y1="16" x2="12" y2="12" />
                    <line x1="12" y1="8" x2="12.01" y2="8" />
                </>
            },
            AlertKind::Success => html! {
                <>
                    <path d="M22 11.08V12a10 10 0 1 1-5.93-9.14" />
                    <polyline points="22 4 12 14.01 9 11.01" />
                </>
            },
        };

        html! {
            <svg
                viewBox="0 0 24 24"
                width="20"
                height="20"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
            >
                { path }
            </svg>
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct AlertProps {
    /// The severity of the alert.
    pub kind: AlertKind,
    /// The short headline shown in bold.
    pub title: AttrValue,
    /// An optional longer description rendered beneath the title.
    #[prop_or_default]
    pub message: Option<AttrValue>,
    /// Invoked when the dismiss affordance is pressed. When absent, the alert is
    /// not dismissible.
    #[prop_or_default]
    pub on_close: Option<Callback<()>>,
    /// Recovery actions (buttons) rendered in the alert's action row.
    #[prop_or_default]
    pub children: Html,
}

/// A prominent, page-level notice used to surface errors and offer the user a
/// way to recover (for example by retrying a failed request or signing in
/// again).
#[function_component(Alert)]
pub fn alert(props: &AlertProps) -> Html {
    let message = match &props.message {
        Some(message) => html! { <p class="alert__message">{ message.clone() }</p> },
        None => html! {},
    };

    let actions = if props.children == Html::default() {
        html! {}
    } else {
        html! { <div class="alert__actions">{ props.children.clone() }</div> }
    };

    let close = match &props.on_close {
        Some(cb) => {
            let cb = cb.clone();
            let onclick = Callback::from(move |_: MouseEvent| cb.emit(()));
            html! {
                <button class="alert__close" aria-label="Dismiss" onclick={onclick}>
                    <svg
                        viewBox="0 0 24 24"
                        width="16"
                        height="16"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="2"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                        aria-hidden="true"
                    >
                        <line x1="18" y1="6" x2="6" y2="18" />
                        <line x1="6" y1="6" x2="18" y2="18" />
                    </svg>
                </button>
            }
        }
        None => html! {},
    };

    html! {
        <div class={classes!("alert", props.kind.class())} role="alert">
            <span class="alert__icon">{ props.kind.icon() }</span>
            <div class="alert__body">
                <span class="alert__title">{ props.title.clone() }</span>
                { message }
                { actions }
            </div>
            { close }
        </div>
    }
}
