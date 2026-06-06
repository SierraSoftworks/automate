use yew::prelude::*;
use yew_router::prelude::*;

use crate::Route;

#[derive(Properties, PartialEq)]
pub struct PageHeaderProps {
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
    /// Invoked when the user chooses to sign out. When absent (for example when
    /// authentication is disabled), the sign-out affordance is hidden.
    #[prop_or_default]
    pub on_signout: Option<Callback<MouseEvent>>,
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

/// The header shown at the top of each admin page, providing navigation back to
/// the dashboard, a title/subtitle, and the signed-in user chip.
#[function_component(PageHeader)]
pub fn page_header(props: &PageHeaderProps) -> Html {
    let back = if props.show_back {
        html! {
            <>
                <Link<Route>
                    to={Route::Dashboard}
                    classes="page-header__back"
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
                </Link<Route>>
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

    let user =
        match &props.user_name {
            Some(name) => {
                let initials = initials(name);
                let email = props.user_email.clone();
                let signout = props.on_signout.as_ref().map(|cb| html! {
                <button class="admin-user-signout" onclick={cb.clone()}>{ "Sign out" }</button>
            });
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
                            { signout }
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
