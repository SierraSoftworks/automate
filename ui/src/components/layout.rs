use chrono::{Datelike, Utc};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct LayoutProps {
    #[prop_or_default]
    pub children: Html,
}

/// The outer page chrome (logo header and footer) shared by every screen. In the
/// client-side app this renders into `<body>`; the stylesheet is loaded by
/// Trunk via the `<link>` in `index.html`.
#[function_component(Layout)]
pub fn layout(props: &LayoutProps) -> Html {
    html! {
        <>
            <div class="header">
                <img
                    src="https://cdn.sierrasoftworks.com/logos/logo.svg"
                    alt="The Sierra Softworks logo."
                />
            </div>

            { props.children.clone() }

            <footer>
                <p>{ format!("Copyright © Sierra Softworks {}", Utc::now().year()) }</p>
            </footer>
        </>
    }
}
