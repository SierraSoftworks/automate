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
use yewi::prelude::*;

use crate::api;
use crate::app::AuthHandle;

/// The entries an administrator's user menu offers for acting as somebody else.
///
/// It renders nothing for anybody who is not an administrator, and nothing
/// when there is nobody to act as — an empty group would only be a heading over
/// nothing. While an administrator is acting as somebody, the way back is the
/// first entry, ahead of the list of other accounts.
#[function_component(ActAsItems)]
pub fn act_as_items() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let accounts = use_state(Vec::<Account>::new);
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
    let others: Vec<&Account> = accounts
        .iter()
        .filter(|account| !account.disabled && Some(&account.username) != current.as_ref())
        .collect();

    let stop = auth.acting_as.is_some().then(|| {
        let on_select = auth.act_as.reform(|_: MouseEvent| None);
        html! {
            <MenuGroup>
                <MenuItem icon={icons::UNDO_2} {on_select}>{ "Back to my own account" }</MenuItem>
            </MenuGroup>
        }
    });

    html! {
        <>
            { stop }
            if !others.is_empty() {
                <MenuGroup label="Act as">
                    { for others.into_iter().map(|account| {
                        let username = account.username.to_string();
                        let on_select = auth.act_as.reform(move |_: MouseEvent| Some(username.clone()));
                        html! {
                            <MenuItem key={account.username.to_string()} {on_select}>
                                { format!("{} ({})", account.display_name, account.username) }
                            </MenuItem>
                        }
                    }) }
                </MenuGroup>
            }
        </>
    }
}

/// The strip announcing that the records on screen belong to somebody else.
///
/// It reads the stored choice rather than the resolved identity, so that it
/// still shows — and can still be dismissed — when the agent refused the choice.
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
    /// The account being acted as.
    pub account: AttrValue,

    /// Fired when the administrator asks to return to their own account.
    pub onstop: Callback<MouseEvent>,
}

/// The notice itself, separate from the context that decides whether to show
/// it so that the control gallery can render one.
#[function_component(ImpersonationNotice)]
pub fn impersonation_notice(props: &ImpersonationNoticeProps) -> Html {
    let actions = html! {
        <Button size={Size::Small} on_click={props.onstop.clone()}>
            { "Back to my own account" }
        </Button>
    };

    html! {
        <Banner
            class="impersonation"
            tone={Tone::Warn}
            icon={icons::USER_ROUND_COG}
            {actions}
            attrs={attrs! { "role" => "status" }}
        >
            { "You are acting as " }
            <strong>{ props.account.clone() }</strong>
            { ". Everything below belongs to that account, and anything you \
               change is recorded against it in your name." }
        </Banner>
    }
}
