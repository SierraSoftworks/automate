use std::rc::Rc;

use chrono::{Datelike, Utc};
use yew::prelude::*;
use yew_router::prelude::{Routable, use_route};
use yewi::prelude::*;

use crate::app::{AuthHandle, AuthStatus, Route};
use crate::components::{ActAsItems, ImpersonationBanner};
use crate::fixtures;
use crate::pages::Protected;
use crate::search::{SearchContext, SearchFilter, SearchVocabulary, VocabularyContext};
use crate::util;

#[derive(Properties, PartialEq)]
pub struct AdminShellProps {
    #[prop_or_default]
    pub children: Html,
}

/// A slot for page-specific actions rendered at the end of the page title row.
/// Pages obtain it from context and push controls (such as a refresh button)
/// into the shared header without owning the title itself.
#[derive(Clone, PartialEq)]
pub struct PageActions {
    set: Callback<Html>,
}

impl PageActions {
    /// Replaces the title-row actions with the given content.
    pub fn set(&self, actions: Html) {
        self.set.emit(actions);
    }

    /// Clears the title-row actions, restoring an empty header.
    pub fn clear(&self) {
        self.set.emit(Html::default());
    }
}

/// The shared chrome for every admin view. It renders the persistent app bar
/// with primary navigation, the page-specific title and toolbar, and gates the
/// routed page behind authentication — all within a single 1280px-wide content
/// column. The search query is provided here so both the app bar input and the
/// routed page can share it.
#[function_component(AdminShell)]
pub fn admin_shell(props: &AdminShellProps) -> Html {
    // The shared search query. It lives here, above both the toolbar (which owns
    // the input) and the routed page (which consumes the parsed filter), so the
    // page's per-second re-render never disturbs the input's focus.
    let query = use_state(String::new);
    let set_query = {
        let query = query.clone();
        use_memo((), move |_| {
            Callback::from(move |value: String| query.set(value))
        })
    };
    let search = SearchContext {
        query: AttrValue::from((*query).clone()),
        filter: Rc::new(SearchFilter::parse(&query)),
        set: (*set_query).clone(),
    };

    // The completion vocabulary (partition names, keys, kinds) lives here too so
    // the search bar can offer value completions for data owned by the routed page.
    // The page publishes it via `VocabularyContext::set`.
    let vocabulary = use_state(|| Rc::new(SearchVocabulary::default()));
    let set_vocabulary = {
        let vocabulary = vocabulary.clone();
        use_memo((), move |_| {
            Callback::from(move |value: SearchVocabulary| vocabulary.set(Rc::new(value)))
        })
    };
    let vocabulary_ctx = VocabularyContext {
        vocabulary: (*vocabulary).clone(),
        set: (*set_vocabulary).clone(),
    };

    // A title-row action slot that routed pages fill via the `PageActions`
    // context (for example with a refresh button). The setter is memoised so the
    // context identity stays stable and pages don't re-render when the actions
    // change.
    let actions = use_state(Html::default);
    let page_actions = {
        let actions = actions.clone();
        use_memo((), move |_| PageActions {
            set: Callback::from(move |content: Html| actions.set(content)),
        })
    };

    // The shell is shared, so what it is a shell *for* has to come from the
    // route. Hard-coding one page's title here meant every other page claimed to
    // be the data browser.
    let route = use_route::<Route>().unwrap_or(Route::Admin);
    {
        let query = query.clone();
        use_effect_with(route.clone(), move |_| {
            query.set(String::new());
            || ()
        });
    }
    let (title, subtitle, search_placeholder, search_autocomplete) = match &route {
        Route::Connections => (
            "Connections",
            "The services this account is linked to, and the credentials used to reach them.",
            "Search connections…",
            false,
        ),
        Route::Workflows => (
            "Workflows",
            "The things Automate watches for you, and what it does when they change.",
            "Search workflows…",
            false,
        ),
        Route::Activity => (
            "Activity",
            "What Automate has been doing for you, and anything that did not work.",
            "Search… (try outcome:failure)",
            true,
        ),
        Route::Users => (
            "Accounts",
            "Everyone who has signed in, and whose records you are looking at.",
            "Search accounts…",
            false,
        ),
        _ => (
            "Admin",
            "Browse the key-value store and job queues across every partition.",
            "Search… (try partition:cron or key:ynab)",
            true,
        ),
    };

    let bar = html! {
        <AppBar brand={brand()} actions={html! { <><ThemeToggle /><UserArea /></> }}>
            <AdminNav />
        </AppBar>
    };

    html! {
        <ContextProvider<SearchContext> context={search}>
            <ContextProvider<VocabularyContext> context={vocabulary_ctx}>
                <AppShell {bar} class="app-shell">
                    <ImpersonationBanner />
                    <div class="app-container">
                        <ContextProvider<PageActions> context={(*page_actions).clone()}>
                            <Protected>
                                <PageHeader title={title} subtitle={html! { { subtitle } }} />
                                <div class="page-toolbar">
                                    <SearchBar
                                        placeholder={search_placeholder}
                                        autocomplete={search_autocomplete}
                                    />
                                    { (*actions).clone() }
                                </div>
                                { props.children.clone() }
                            </Protected>
                        </ContextProvider<PageActions>>
                    </div>
                    <Footer />
                </AppShell>
            </ContextProvider<VocabularyContext>>
        </ContextProvider<SearchContext>>
    }
}

/// The brand mark at the start of the app bar, linking back to the admin home.
pub fn brand() -> Html {
    let logo = html! {
        <img src="https://cdn.sierrasoftworks.com/logos/icon.svg" alt="" />
    };

    html! {
        <AppBarBrand {logo} href={util::nav_href("/admin")}>{ "Automate" }</AppBarBrand>
    }
}

/// The copyright line at the foot of every page.
#[function_component(Footer)]
pub fn footer() -> Html {
    html! {
        <footer class="app-footer">
            <Text tag={TextTag::P} size={TextSize::Sm} color={TextColor::Subtle}>
                { format!("Copyright © Sierra Softworks {}", Utc::now().year()) }
            </Text>
        </footer>
    }
}

/// The signed-in user at the end of the app bar: who they are, the accounts an
/// administrator may act as, and the way out.
#[function_component(UserArea)]
fn user_area() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let Some(user) = auth.user.clone() else {
        return html! {};
    };

    let on_sign_out = auth.signout.reform(|_| ());

    html! {
        <UserMenu
            name={user.name.clone()}
            detail={user.email.clone().map(AttrValue::from)}
            show_name=true
            {on_sign_out}
        >
            <ActAsItems />
        </UserMenu>
    }
}

#[derive(Properties, PartialEq)]
pub struct SearchBarProps {
    pub placeholder: AttrValue,

    /// Whether the page publishes a vocabulary worth completing against. Pages
    /// that only do free-text matching leave the suggestions off.
    #[prop_or_default]
    pub autocomplete: bool,
}

/// The unified search field shared by the routed admin pages.
///
/// A bare term matches everything a row shows; `field:value` scopes a term to
/// one property. The field names and their values come from the routed page,
/// through [`VocabularyContext`], and yewi's `QueryInput` offers them as the
/// user types.
#[function_component(SearchBar)]
pub fn search_bar(props: &SearchBarProps) -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let search = use_context::<SearchContext>();
    let vocabulary = use_context::<VocabularyContext>();

    let signed_in = matches!(auth.status, AuthStatus::SignedIn(_) | AuthStatus::Disabled);
    let (true, Some(search)) = (signed_in, search) else {
        return html! { <div class="page-toolbar__spacer" /> };
    };

    let fields: Rc<[QueryField]> = match vocabulary.filter(|_| props.autocomplete) {
        Some(ctx) => ctx
            .vocabulary
            .fields
            .iter()
            .map(|field| {
                QueryField::new(field.name.clone())
                    .description(field.description.clone())
                    .values(field.values.iter().cloned())
            })
            .collect(),
        None => Rc::from([]),
    };

    html! {
        <QueryInput
            class="page-toolbar__search"
            value={search.query.clone()}
            on_input={search.set.clone()}
            {fields}
            label="Search"
            placeholder={props.placeholder.clone()}
        />
    }
}

/// The links between the admin pages.
#[function_component(AdminNav)]
fn admin_nav() -> Html {
    let current = use_route::<Route>().unwrap_or(Route::Admin);
    let auth = use_context::<AuthHandle>();

    let link = |route: Route, label: &'static str| {
        // Both spellings of the browser route are the same destination, so one
        // must not appear unselected while the other is showing.
        let active = matches!(
            (&route, &current),
            (Route::Connections, Route::Connections)
                | (Route::Workflows, Route::Workflows)
                | (Route::Activity, Route::Activity)
                | (Route::Users, Route::Users)
                | (Route::Admin, Route::Admin | Route::AdminRoot)
        );

        // Demo mode lives in the query string, and a client-side navigation
        // replaces the whole URL — so following a link out of a demo page would
        // otherwise land on one talking to an agent that is not there.
        let href = if fixtures::is_demo() {
            NavTarget::href(util::nav_href(&route.to_path()))
        } else {
            NavTarget::route(route)
        };

        html! { <TopMenuItem {href} current={active}>{ label }</TopMenuItem> }
    };

    // Nobody else can load the page behind it, so offering the link would only
    // be a promise the agent then refuses.
    let is_admin = auth
        .and_then(|auth| auth.user)
        .is_some_and(|user| user.is_admin);

    html! {
        <TopMenu label="Admin pages">
            { link(Route::Workflows, "Workflows") }
            { link(Route::Connections, "Connections") }
            { link(Route::Activity, "Activity") }
            { link(Route::Admin, "Data") }
            if is_admin {
                { link(Route::Users, "Accounts") }
            }
            // Only reachable in demo mode, which is the only mode it works in.
            if fixtures::is_demo() {
                <TopMenuItem href={util::nav_href("/demo/controls")}>{ "Controls" }</TopMenuItem>
            }
        </TopMenu>
    }
}
