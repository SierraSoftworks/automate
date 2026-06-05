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
    #[prop_or_default]
    pub subtitle: Option<AttrValue>,
    /// Whether to render the "Back" affordance (hidden on the dashboard root).
    #[prop_or(true)]
    pub show_back: bool,
    /// The display name of the signed-in user, when OIDC authentication is
    /// enabled.
    #[prop_or_default]
    pub user_name: Option<AttrValue>,
    /// The email address of the signed-in user, when available.
    #[prop_or_default]
    pub user_email: Option<AttrValue>,
}

/// Derives up to two uppercase initials from a display name or email address.
fn initials(name: &str) -> String {
    let from_words: String = name
        .split(|c: char| c.is_whitespace() || c == '.' || c == '@' || c == '_' || c == '-')
        .filter(|w| !w.is_empty())
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect();

    let initials = if from_words.is_empty() {
        name.chars().take(2).collect()
    } else {
        from_words
    };

    initials.to_uppercase()
}

#[function_component(AdminHeader)]
pub fn admin_header(props: &AdminHeaderProps) -> Html {
    let back = if props.show_back {
        html! {
            <>
                <a
                    class="page-header__back"
                    href="/admin"
                    aria-label="Back to admin dashboard"
                    title="Back to admin dashboard"
                >
                    <span class="page-header__icon">
                        <svg
                            viewBox="0 0 24 24"
                            width="18"
                            height="18"
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
                    </span>
                    <span class="page-header__back-text">{ "Back" }</span>
                </a>
                <div class="page-header__divider" />
            </>
        }
    } else {
        html! {}
    };

    let subtitle = match &props.subtitle {
        Some(subtitle) => html! {
            <span class="page-header__subtitle">{ subtitle.clone() }</span>
        },
        None => html! {},
    };

    let user = match &props.user_name {
        Some(name) => {
            let initials = initials(name);
            let email = props.user_email.clone();
            html! {
                <div class="page-header__extra">
                    <div class="admin-user">
                        <span class="admin-user-avatar">{ initials }</span>
                        <span class="admin-user-meta">
                            <span class="admin-user-name">{ name.clone() }</span>
                            {
                                match email {
                                    Some(email) => html! {
                                        <span class="admin-user-email">{ email }</span>
                                    },
                                    None => html! {},
                                }
                            }
                        </span>
                    </div>
                </div>
            }
        }
        None => html! {},
    };

    html! {
        <div class="page-header">
            <div class="page-header__main">
                { back }
                <div class="page-header__content">
                    <span class="page-header__heading">{ props.title.clone() }</span>
                    { subtitle }
                </div>
            </div>
            { user }
        </div>
    }
}
