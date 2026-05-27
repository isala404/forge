mod forge;
mod todo_app;
mod todo_item;

use dioxus::prelude::*;
use forge::ForgeAuthProvider;
use todo_app::TodoApp;

fn api_url() -> &'static str {
    option_env!("FORGE_API_URL").unwrap_or("http://localhost:9081")
}

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        document::Title { "Todos" }
        document::Stylesheet { href: asset!("/public/style.css") }
        ForgeAuthProvider {
            url: api_url().to_string(),
            app_name: "todo-dioxus".to_string(),
            TodoApp {}
        }
    }
}
