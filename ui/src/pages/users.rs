//! Every account in the installation, for an administrator to look after.
//!
//! Two things happen here, and they are not the same thing. Suspending an
//! account is the one lever that works without editing the configuration file,
//! so it belongs somewhere findable. Acting as an account is how somebody gets
//! *helped* — it puts the administrator in that person's records, which is the
//! only way to see what they are describing.
//!
//! The installation's own account appears alongside the people. Nobody signs
//! into it, but it owns everything that predates multi-tenancy being switched
//! on, so leaving it out would make those records unreachable from the browser
//! entirely.

use automate_api::Account;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::app::AuthHandle;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, PageActions, RefreshButton, StatusPill, StatusTone,
};
use crate::search::{MatchContext, SearchContext};
use crate::util::short_relative;

#[function_component(Users)]
pub fn users() -> Html {
    let accounts = use_state(|| None::<Vec<Account>>);
    let error = use_state(|| None::<String>);
    let refreshing = use_state(|| false);
    let reload = use_state(|| 0u32);

    {
        let accounts = accounts.clone();
        let error = error.clone();
        use_effect_with(*reload, move |_| {
            spawn_local(async move {
                match api::list_accounts().await {
                    Ok(loaded) => {
                        accounts.set(Some(loaded));
                        error.set(None);
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
            });
        });
    }

    let refresh = {
        let reload = reload.clone();
        let refreshing = refreshing.clone();
        Callback::from(move |_: ()| {
            refreshing.set(true);
            reload.set(*reload + 1);
            refreshing.set(false);
        })
    };

    let page_actions = use_context::<PageActions>();
    {
        let page_actions = page_actions.clone();
        let refresh = refresh.clone();
        let refreshing = *refreshing;
        use_effect_with(refreshing, move |_| {
            if let Some(actions) = &page_actions {
                actions.set(html! {
                    <RefreshButton onclick={Callback::from(move |_: MouseEvent| refresh.emit(()))} busy={refreshing} />
                });
            }
            move || {
                if let Some(actions) = page_actions {
                    actions.clear();
                }
            }
        });
    }

    let on_changed = {
        let reload = reload.clone();
        Callback::from(move |_| reload.set(*reload + 1))
    };

    let search = use_context::<SearchContext>();
    let filter = search
        .as_ref()
        .map(|search| search.filter.clone())
        .unwrap_or_default();

    html! {
        <section class="accounts-page">
            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not load the accounts."
                    message={message.clone()}
                />
            }

            {
                match &*accounts {
                    None => html! { <p class="accounts__empty">{ "Loading…" }</p> },
                    Some(list) => {
                        let visible: Vec<&Account> = list
                            .iter()
                            .filter(|account| {
                                let text = format!(
                                    "{} {} {}",
                                    account.display_name,
                                    account.username,
                                    account.email.as_deref().unwrap_or_default(),
                                )
                                .to_lowercase();
                                filter.matches(&MatchContext {
                                    fields: &[
                                        ("name", &account.display_name),
                                        ("account", account.username.as_str()),
                                        ("status", if account.disabled { "suspended" } else { "active" }),
                                    ],
                                    text: &text,
                                })
                            })
                            .collect();

                        if visible.is_empty() {
                            html! { <p class="accounts__empty">{ "No accounts match your search." }</p> }
                        } else {
                            html! {
                                <ul class="accounts__list">
                                    { for visible.into_iter().map(|account| html! {
                                        <li key={account.username.to_string()}>
                                            <AccountRow
                                                account={account.clone()}
                                                on_changed={on_changed.clone()}
                                            />
                                        </li>
                                    }) }
                                </ul>
                            }
                        }
                    },
                }
            }
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct AccountRowProps {
    account: Account,
    on_changed: Callback<()>,
}

#[function_component(AccountRow)]
fn account_row(props: &AccountRowProps) -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let account = &props.account;
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    // The account this browser is currently reaching, which is the impersonated
    // one when there is one.
    let current = auth.user.as_ref().and_then(|user| user.username.clone());
    let is_current = current.as_ref() == Some(&account.username);

    let on_act_as = {
        let act_as = auth.act_as.clone();
        let username = account.username.to_string();
        Callback::from(move |_: MouseEvent| act_as.emit(Some(username.clone())))
    };

    let on_toggle_disabled = {
        let username = account.username.to_string();
        let disabled = !account.disabled;
        let on_changed = props.on_changed.clone();
        let (busy, error) = (busy.clone(), error.clone());
        Callback::from(move |_: MouseEvent| {
            let username = username.clone();
            let on_changed = on_changed.clone();
            let (busy, error) = (busy.clone(), error.clone());
            busy.set(true);
            spawn_local(async move {
                match api::set_account_disabled(&username, disabled).await {
                    Ok(_) => on_changed.emit(()),
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let (tone, status) = if account.disabled {
        (StatusTone::Error, "Suspended")
    } else if account.reserved {
        (StatusTone::Neutral, "Installation")
    } else {
        (StatusTone::Ok, "Active")
    };

    let (suspend_label, suspend_kind) = if account.disabled {
        (html! { { "Restore" } }, ButtonKind::Default)
    } else {
        (html! { { "Suspend" } }, ButtonKind::Danger)
    };

    html! {
        <div class="account">
            <div class="account__identity">
                <span class="account__name">{ &account.display_name }</span>
                <span class="account__username">{ account.username.to_string() }</span>
                if let Some(email) = &account.email {
                    <span class="account__email">{ email }</span>
                }
            </div>

            <div class="account__facts">
                <StatusPill tone={tone} label={status} />
                if account.is_admin {
                    <StatusPill
                        tone={StatusTone::Neutral}
                        label="Administrator"
                        title="Matched the admin filter when they last signed in."
                    />
                }
                <span class="account__seen">
                    {
                        match account.last_seen_at {
                            Some(seen) => format!("Last seen {}", short_relative(seen)),
                            None => "Nobody signs into this account".to_string(),
                        }
                    }
                </span>
            </div>

            <div class="account__actions">
                if is_current {
                    <span class="account__here">{ "You are here" }</span>
                } else if !account.disabled {
                    <Button kind={ButtonKind::Default} small=true onclick={on_act_as}>
                        { "Act as" }
                    </Button>
                }
                // The installation's own account has nobody to lock out, and
                // suspending it would only put its records out of reach.
                if !account.reserved && !is_current {
                    <Button
                        kind={suspend_kind}
                        small=true
                        disabled={*busy}
                        onclick={on_toggle_disabled}
                    >
                        { suspend_label }
                    </Button>
                }
            </div>

            if let Some(message) = &*error {
                <p class="account__error">{ message }</p>
            }
        </div>
    }
}
