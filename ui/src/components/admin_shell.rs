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

    html! {
        <div class="app-shell">
            <AppBar active={route} />
            <main class="app-main">
                <div class="app-container">
                    <Protected>
                        <PageTitle title={title} subtitle={subtitle.map(AttrValue::from)} />
                        { props.children.clone() }
                    </Protected>
                </div>
            </main>
            <footer class="app-footer">
                <p>{ format!("Copyright © Sierra Softworks {}", Utc::now().year()) }</p>
            </footer>
        </div>
    }
}
