use yew::prelude::*;
use yewi::prelude::*;

#[derive(Properties, PartialEq)]
pub struct RefreshButtonProps {
    pub onclick: Callback<MouseEvent>,

    /// Whether a refresh is in flight. The button shows a spinner and ignores
    /// clicks until it finishes.
    #[prop_or_default]
    pub busy: bool,
}

/// The "Refresh" action pages put in the toolbar beside the search field.
#[function_component(RefreshButton)]
pub fn refresh_button(props: &RefreshButtonProps) -> Html {
    html! {
        <Button
            variant={ButtonVariant::Primary}
            icon={icons::REFRESH_CW}
            loading={props.busy}
            on_click={props.onclick.clone()}
        >
            { "Refresh" }
        </Button>
    }
}
