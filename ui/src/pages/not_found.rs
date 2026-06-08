use yew::prelude::*;
use yew_router::prelude::*;

use crate::Route;
use crate::components::{Center, Layout};

/// Fallback page shown for unmatched routes.
#[function_component(NotFound)]
pub fn not_found() -> Html {
    html! {
        <Layout>
            <Center>
                <div class="auth-card">
                    <h1 class="auth-card__title">{ "Page not found" }</h1>
                    <p class="auth-card__lead">{ "The page you were looking for doesn't exist." }</p>
                    <Link<Route> to={Route::Landing} classes="btn btn--primary btn--lg">
                        { "Back to home" }
                    </Link<Route>>
                </div>
            </Center>
        </Layout>
    }
}
