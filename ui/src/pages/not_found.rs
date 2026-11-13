use yew::prelude::*;
use yewi::prelude::*;

use crate::Route;
use crate::components::{Footer, brand};

/// Fallback page shown for unmatched routes.
#[function_component(NotFound)]
pub fn not_found() -> Html {
    html! {
        <AppShell bar={html! { <AppBar brand={brand()} actions={html! { <ThemeToggle /> }} /> }} class="app-shell app-shell--bare">
            <EmptyState
                class="auth-screen"
                title="Page not found"
                icon={icons::COMPASS}
                actions={html! {
                    <Button variant={ButtonVariant::Primary} href={NavTarget::route(Route::Landing)}>
                        { "Back to home" }
                    </Button>
                }}
            >
                { "The page you were looking for doesn't exist." }
            </EmptyState>
            <Footer />
        </AppShell>
    }
}
