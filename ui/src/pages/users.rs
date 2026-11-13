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
use yewi::prelude::*;

use crate::components::{PageActions, RefreshButton};
use crate::search::{MatchContext, SearchContext};

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
                <Alert tone={Tone::Danger} title="We could not load the accounts.">
                    { message.clone() }
                </Alert>
            }

            {
                match &*accounts {
                    None => html! { <Loading label="Loading…" layout={LoadingLayout::Block} /> },
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
                            html! { <EmptyState title="No accounts match your search." icon={icons::SEARCH_X} /> }
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
        (Tone::Danger, "Suspended")
    } else if account.reserved {
        (Tone::Neutral, "Installation")
    } else {
        (Tone::Ok, "Active")
    };

    let (suspend_label, suspend_variant) = if account.disabled {
        ("Restore", ButtonVariant::Default)
    } else {
        ("Suspend", ButtonVariant::Danger)
    };

    html! {
        <div class="account">
            <UserChip
                class="account__identity"
                name={account.display_name.clone()}
                detail={match &account.email {
                    Some(email) => format!("{} · {email}", account.username),
                    None => account.username.to_string(),
                }}
            />
            <div class="account__facts">
                <Pill tone={tone} dot=true>{ status }</Pill>
                if account.is_admin {
                    <Tag attrs={attrs! { "title" => "Matched the admin filter when they last signed in." }}>
                        { "Administrator" }
                    </Tag>
                }
                <Text size={TextSize::Sm} color={TextColor::Muted} class="account__seen">
                    {
                        match account.last_seen_at {
                            Some(seen) => html! { <>{ "Last seen " }<Timestamp at={seen} compact=true /></> },
                            None => html! { { "Nobody signs into this account" } },
                        }
                    }
                </Text>
            </div>
            <div class="account__actions">
                if is_current {
                    <Text size={TextSize::Sm} color={TextColor::Muted}>{ "You are here" }</Text>
                } else if !account.disabled {
                    <Button size={Size::Small} on_click={on_act_as}>{ "Act as" }</Button>
                }
                if !account.reserved && !is_current {
                    <Button
                        variant={suspend_variant}
                        size={Size::Small}
                        disabled={*busy}
                        on_click={on_toggle_disabled}
                    >
                        { suspend_label }
                    </Button>
                }
            </div>
            if let Some(message) = &*error {
                <Text tag={TextTag::P} size={TextSize::Sm} tone={Tone::Danger} class="account__error">
                    { message }
                </Text>
            }
        </div>
    }
}
