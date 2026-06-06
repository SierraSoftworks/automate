use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct CardProps {
    /// The destination the card links to when clicked.
    pub href: AttrValue,
    /// The card's primary heading.
    pub title: AttrValue,
    /// A short supporting description rendered beneath the title.
    pub description: AttrValue,
    /// The icon rendered to the left of the card body (typically an SVG).
    #[prop_or_default]
    pub icon: Html,
}

/// A clickable dashboard card used to navigate to an admin section.
#[function_component(Card)]
pub fn card(props: &CardProps) -> Html {
    html! {
        <a class="admin-card" href={props.href.clone()}>
            <span class="admin-card-icon">{ props.icon.clone() }</span>
            <span class="admin-card-body">
                <span class="admin-card-title">{ props.title.clone() }</span>
                <span class="admin-card-desc">{ props.description.clone() }</span>
            </span>
        </a>
    }
}
