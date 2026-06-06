mod api;
mod app;
mod auth;
mod components;
mod fixtures;
mod pages;
mod util;

pub use app::Route;

fn main() {
    console_error_panic_hook::set_once();
    wasm_logger::init(wasm_logger::Config::default());
    yew::Renderer::<app::App>::new().render();
}
