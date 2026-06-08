use chrono::{Datelike, Utc};
use yew::prelude::*;
use yew_router::prelude::*;

use crate::Route;
use crate::components::{AppBar, PageTitle};
use crate::pages::Protected;

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

/// Maps a route to the page title and supporting subtitle shown in the
/// page-specific header.
fn context_for(route: &Route) -> (&'static str, Option<&'static str>) {
    match route {
        Route::Db => (
            "Key-Value Store",
            Some("Persistent state used by collectors and publishers."),
        ),
        Route::Queue => (
            "Queue",
            Some("Pending jobs awaiting execution, grouped by partition."),
        ),
        _ => (
            "Dashboard",
            Some("Inspect the persistent state that drives Automate's workflows."),
        ),
    }
}

/// The shared chrome for every admin view. It renders the persistent app bar,
/// the page-specific title, and gates the routed page behind authentication —
/// all within a single 1280px-wide content column.
#[function_component(AdminShell)]
pub fn admin_shell(props: &AdminShellProps) -> Html {
    let route = match use_route::<Route>() {
        Some(Route::AdminRoot) => Route::Dashboard,
        Some(route) => route,
        None => Route::Dashboard,
    };
    let (title, subtitle) = context_for(&route);

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

    html! {
        <div class="app-shell">
            <AppBar active={route} />
            <main class="app-main">
                <div class="app-container">
                    <ContextProvider<PageActions> context={(*page_actions).clone()}>
                        <Protected>
                            <PageTitle title={title} subtitle={subtitle.map(AttrValue::from)}>
                                { (*actions).clone() }
                            </PageTitle>
                            { props.children.clone() }
                        </Protected>
                    </ContextProvider<PageActions>>
                </div>
            </main>
            <footer class="app-footer">
                <p>{ format!("Copyright © Sierra Softworks {}", Utc::now().year()) }</p>
            </footer>
        </div>
    }
}
