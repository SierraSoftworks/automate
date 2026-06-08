use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct RefreshButtonProps {
    /// Invoked when the button is activated to re-fetch the page's data.
    pub onclick: Callback<MouseEvent>,
    /// When true the button is disabled and its icon spins to signal that a
    /// refresh is already in progress.
    #[prop_or_default]
    pub busy: bool,
}

/// A compact control that re-fetches a page's data in place, without reloading
/// the whole UI. The icon spins while a refresh is in flight.
#[function_component(RefreshButton)]
pub fn refresh_button(props: &RefreshButtonProps) -> Html {
    let mut icon_class = classes!("refresh-btn__icon");
    if props.busy {
        icon_class.push("refresh-btn__icon--spin");
    }

    html! {
        <button
            class="btn btn--small"
            onclick={props.onclick.clone()}
            disabled={props.busy}
            title="Refresh"
            aria-label="Refresh"
        >
            <span class={icon_class} aria-hidden="true">
                <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor"
                    stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <polyline points="23 4 23 10 17 10" />
                    <polyline points="1 20 1 14 7 14" />
                    <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
                </svg>
            </span>
            { "Refresh" }
        </button>
    }
}
