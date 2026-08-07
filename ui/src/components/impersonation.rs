//! Acting as another account.
//!
//! An administrator helping somebody has to see what that person sees, and the
//! agent already supports it: an `X-Impersonate-User` header redirects which
//! account a request reaches while leaving permissions and attribution with
//! whoever signed in. What was missing was any way to ask for it from the
//! browser.
//!
//! Two controls, deliberately in different places. Choosing an account is a
//! command like any other, so it lives in the menu on the user chip. Being in
//! somebody else's account is a *state*, and a state that makes every other
//! screen mean something different — so it is announced by a banner across the
//! top of the shell rather than by a subtly different chip, and the way out of
//! it is in the banner where somebody looking for it will be looking.

use automate_api::Account;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::app::AuthHandle;
use crate::components::{MenuButton, MenuButtonOption};

/// The menu value that means "go back to my own account". A sentinel rather
/// than an empty string so that it cannot collide with an account name.
const STOP: &str = "\u{0}stop";

/// The "act as" menu, shown only to administrators.
///
/// Suspended accounts are left out because the agent refuses to act as one, and
/// offering a choice that always fails is worse than not offering it.
#[function_component(ActAsMenu)]
pub fn act_as_menu() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let accounts = use_state(Vec::<Account>::new);

    // `is_admin` describes whoever signed in, not the account being acted as, so
    // this stays available while impersonating — which is what lets somebody
    // switch straight from one account to another.
    let is_admin = auth.user.as_ref().is_some_and(|user| user.is_admin);

    {
        let accounts = accounts.clone();
        use_effect_with(is_admin, move |is_admin| {
            if *is_admin {
                spawn_local(async move {
                    if let Ok(loaded) = api::list_accounts().await {
                        accounts.set(loaded);
                    }
                });
            }
            || ()
        });
    }

    if !is_admin {
        return html! {};
    }

    let current = auth.user.as_ref().and_then(|user| user.username.clone());

    let mut options: Vec<MenuButtonOption> = accounts
        .iter()
        .filter(|account| !account.disabled && Some(&account.username) != current.as_ref())
        .map(|account| {
            MenuButtonOption::new(
                account.username.to_string(),
                format!("{} ({})", account.display_name, account.username),
            )
            .in_section("Act as")
        })
        .collect();

    if auth.acting_as.is_some() {
        options.insert(0, MenuButtonOption::new(STOP, "Back to my own account"));
    }

    if options.is_empty() {
        return html! {};
    }

    let onselect = {
        let act_as = auth.act_as.clone();
        Callback::from(move |value: String| act_as.emit((value != STOP).then_some(value)))
    };

    html! {
        <MenuButton
            label="Act as…"
            options={options}
            onselect={onselect}
            small=true
            title="Act as another account"
        />
    }
}

/// The banner announcing that the records on screen are somebody else's.
///
/// Split from [`ImpersonationNotice`] so that the thing being looked at in the
/// gallery is the same markup the shell renders, rather than a copy of it: this
/// half only decides *whether* there is anything to say.
#[function_component(ImpersonationBanner)]
pub fn impersonation_banner() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");

    let Some(account) = auth.acting_as.clone() else {
        return html! {};
    };

    let onstop = {
        let act_as = auth.act_as.clone();
        Callback::from(move |_: MouseEvent| act_as.emit(None))
    };

    html! { <ImpersonationNotice account={account} onstop={onstop} /> }
}

#[derive(Properties, PartialEq)]
pub struct ImpersonationNoticeProps {
    /// The account whose records are on screen.
    pub account: AttrValue,
    pub onstop: Callback<MouseEvent>,
}

#[function_component(ImpersonationNotice)]
pub fn impersonation_notice(props: &ImpersonationNoticeProps) -> Html {
    html! {
        <div class="impersonation" role="status">
            <p class="impersonation__message">
                { "You are acting as " }
                <strong>{ props.account.clone() }</strong>
                { ". Everything below belongs to that account, and anything you \
                   change is recorded against it in your name." }
            </p>
            <button class="btn btn--small impersonation__stop" onclick={props.onstop.clone()}>
                { "Back to my own account" }
            </button>
        </div>
    }
}
