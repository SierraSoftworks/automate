use yew::prelude::*;
use yew_router::prelude::*;

use crate::Route;
use crate::components::Center;

/// Fallback page shown for unmatched routes.
#[function_component(NotFound)]
pub fn not_found() -> Html {
    html! {
        <Center>
            <div class="auth-card">
                <h1>{ "Page not found" }</h1>
                <p>{ "The page you were looking for doesn't exist." }</p>
                <Link<Route> to={Route::Dashboard}>
                    <button>{ "Back to dashboard" }</button>
                </Link<Route>>
            </div>
        </Center>
    }
}
