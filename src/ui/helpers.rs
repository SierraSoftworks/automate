use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct CenterProps {
    pub children: Html,
}

#[function_component(Center)]
pub fn center(props: &CenterProps) -> Html {
    html! {
        <div style="position: absolute; top: 50%; left: 50%; transform: translate(-50%, -50%);">
            {props.children.clone()}
        </div>
    }
}
