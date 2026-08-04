use std::rc::Rc;

use chrono::{Datelike, Utc};
use yew::prelude::*;

use yew_router::prelude::*;

use crate::app::Route;
use crate::components::{AppBar, PageTitle};
use crate::pages::Protected;
use crate::search::{SearchContext, SearchFilter, SearchVocabulary, VocabularyContext};

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
/// (which hosts the unified search), the page-specific title, and gates the
/// routed page behind authentication — all within a single 1280px-wide content
/// column. The search query is provided here so both the app bar input and the
/// routed page can share it.
#[function_component(AdminShell)]
pub fn admin_shell(props: &AdminShellProps) -> Html {
    // The shared search query. It lives here, above both the app bar (which owns
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
    // the app bar can offer value completions for data owned by the routed page.
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
    let (title, subtitle) = match use_route::<Route>().unwrap_or(Route::Admin) {
        Route::Connections => (
            "Connections",
            "The services this account is linked to, and the credentials used to reach them.",
        ),
        Route::Workflows => (
            "Workflows",
            "The things Automate watches for you, and what it does when they change.",
        ),
        _ => (
            "Admin",
            "Browse the key-value store and job queues across every partition.",
        ),
    };

    html! {
        <ContextProvider<SearchContext> context={search}>
            <ContextProvider<VocabularyContext> context={vocabulary_ctx}>
                <div class="app-shell">
                    <AppBar />
                    <main class="app-main">
                        <div class="app-container">
                            <ContextProvider<PageActions> context={(*page_actions).clone()}>
                                <Protected>
                                    <PageTitle title={title} subtitle={subtitle}>
                                        { (*actions).clone() }
                                    </PageTitle>
                                    <AdminNav />
                                    { props.children.clone() }
                                </Protected>
                            </ContextProvider<PageActions>>
                        </div>
                    </main>
                    <footer class="app-footer">
                        <p>{ format!("Copyright © Sierra Softworks {}", Utc::now().year()) }</p>
                    </footer>
                </div>
            </ContextProvider<VocabularyContext>>
        </ContextProvider<SearchContext>>
    }
}

/// The links between the admin pages.
///
/// Small enough to live beside the shell that renders it: there are three
/// destinations, and giving them their own module would be more ceremony than
/// the thing deserves.
#[function_component(AdminNav)]
fn admin_nav() -> Html {
    let current = use_route::<Route>().unwrap_or(Route::Admin);

    let link = |route: Route, label: &'static str| {
        // Both spellings of the browser route are the same destination, so one
        // must not appear unselected while the other is showing.
        let active = matches!(
            (&route, &current),
            (Route::Connections, Route::Connections)
                | (Route::Workflows, Route::Workflows)
                | (Route::Admin, Route::Admin | Route::AdminRoot)
        );

        html! {
            <Link<Route>
                to={route}
                classes={classes!("admin-nav__link", active.then_some("admin-nav__link--active"))}
            >
                { label }
            </Link<Route>>
        }
    };

    html! {
        <nav class="admin-nav">
            { link(Route::Workflows, "Workflows") }
            { link(Route::Connections, "Connections") }
            { link(Route::Admin, "Data") }
        </nav>
    }
}
