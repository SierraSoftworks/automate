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

#[derive(Properties, PartialEq)]
pub struct AdminHeaderProps {
    pub title: AttrValue,
}

#[function_component(AdminHeader)]
pub fn admin_header(props: &AdminHeaderProps) -> Html {
    html! {
        <div class="admin-page-header">
            <a
                class="admin-back"
                href="/admin"
                aria-label="Back to admin dashboard"
                title="Back to admin dashboard"
            >
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
                    <line x1="19" y1="12" x2="5" y2="12" />
                    <polyline points="12 19 5 12 12 5" />
                </svg>
            </a>
            <h1 class="admin-page-title">{ props.title.clone() }</h1>
        </div>
    }
}

