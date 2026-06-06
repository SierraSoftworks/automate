use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct CenterProps {
    pub children: Html,
}

/// Absolutely centres its children within the viewport. Used for login and
/// status screens.
#[function_component(Center)]
pub fn center(props: &CenterProps) -> Html {
    html! {
        <div class="center-screen">
            { props.children.clone() }
        </div>
    }
}
