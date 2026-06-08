use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct PageTitleProps {
    /// The page's primary heading, reflecting the active route.
    pub title: AttrValue,
    /// An optional supporting line describing the page's purpose.
    #[prop_or_default]
    pub subtitle: Option<AttrValue>,
    /// Optional controls (search, actions) aligned to the end of the title row.
    #[prop_or_default]
    pub children: Html,
}

/// The page-specific context shown beneath the persistent app bar. It renders
/// the active page's title and an optional descriptive subtitle within the
/// shared content container.
#[function_component(PageTitle)]
pub fn page_title(props: &PageTitleProps) -> Html {
    let subtitle = match &props.subtitle {
        Some(subtitle) => html! { <p class="page-title__subtitle">{ subtitle.clone() }</p> },
        None => html! {},
    };

    html! {
        <div class="page-title">
            <div class="page-title__text">
                <h1 class="page-title__heading">{ props.title.clone() }</h1>
                { subtitle }
            </div>
            { props.children.clone() }
        </div>
    }
}
