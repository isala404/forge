use dioxus::prelude::*;
use forge_dioxus::use_signals;
use serde_json::json;

use crate::forge::{
    CreateTodoInput, LoginInput, RegisterInput, UserPublic, use_create_todo, use_forge_auth,
    use_list_todos_subscription, use_login, use_register,
};
use crate::todo_item::TodoItem;

#[component]
pub fn TodoApp() -> Element {
    let auth = use_forge_auth();

    rsx! {
        main {
            div { class: "shell",
                header { class: "hero",
                    h1 { "Todos" }
                    if auth.is_authenticated() {
                        UserBar {}
                    }
                }
                if auth.is_authenticated() {
                    TodoList {}
                } else {
                    AuthPanel {}
                }
            }
        }
    }
}

#[component]
fn UserBar() -> Element {
    let mut auth = use_forge_auth();
    let viewer = auth.viewer::<UserPublic>();
    let label = viewer
        .as_ref()
        .map(|u| u.name.clone())
        .unwrap_or_default();

    rsx! {
        div { class: "user-row",
            span { class: "user", "{label}" }
            button {
                class: "logout",
                onclick: move |_| auth.logout(),
                "Sign out"
            }
        }
    }
}

#[component]
fn AuthPanel() -> Element {
    let mut auth = use_forge_auth();
    let signals = use_signals();
    let login_mut = use_login();
    let register_mut = use_register();

    let mut mode = use_signal(|| "login".to_string());
    let mut email = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let mut loading = use_signal(|| false);

    let handle_submit = {
        let login_mut = login_mut.clone();
        let register_mut = register_mut.clone();
        let signals = signals.clone();
        move |evt: FormEvent| {
            evt.prevent_default();
            let is_register = mode.read().as_str() == "register";
            let e = email.read().clone();
            let n = name.read().clone();
            let p = password.read().clone();
            let login_mut = login_mut.clone();
            let register_mut = register_mut.clone();
            let signals = signals.clone();
            spawn(async move {
                loading.set(true);
                error.set(None);
                let res = if is_register {
                    register_mut.call(RegisterInput::new(&e, &n, &p)).await
                } else {
                    login_mut.call(LoginInput::new(&e, &p)).await
                };
                match res {
                    Ok(r) => {
                        signals.track_with_properties(
                            "auth_success",
                            json!({"mode": is_register}),
                        );
                        auth.login_with_viewer(
                            r.access_token.clone(),
                            r.refresh_token.clone(),
                            &r.user,
                        );
                    }
                    Err(e) => error.set(Some(e.message)),
                }
                loading.set(false);
            });
        }
    };

    rsx! {
        section { class: "auth-panel",
            div { class: "tabs",
                button {
                    class: if mode.read().as_str() == "login" { "active" } else { "" },
                    onclick: move |_| mode.set("login".into()),
                    "Sign in"
                }
                button {
                    class: if mode.read().as_str() == "register" { "active" } else { "" },
                    onclick: move |_| mode.set("register".into()),
                    "Sign up"
                }
            }
            form { onsubmit: handle_submit,
                if mode.read().as_str() == "register" {
                    input {
                        r#type: "text",
                        placeholder: "Name",
                        value: "{name}",
                        oninput: move |e: FormEvent| name.set(e.value()),
                        required: true,
                    }
                }
                input {
                    r#type: "email",
                    placeholder: "Email",
                    value: "{email}",
                    oninput: move |e: FormEvent| email.set(e.value()),
                    required: true,
                }
                input {
                    r#type: "password",
                    placeholder: "Password (min 8 chars)",
                    value: "{password}",
                    oninput: move |e: FormEvent| password.set(e.value()),
                    minlength: "8",
                    required: true,
                }
                button {
                    r#type: "submit",
                    disabled: loading(),
                    if loading() { "..." } else if mode.read().as_str() == "login" { "Sign in" } else { "Sign up" }
                }
            }
            if let Some(msg) = error() {
                p { class: "error", "{msg}" }
            }
        }
    }
}

#[component]
fn TodoList() -> Element {
    let signals = use_signals();
    let create_todo = use_create_todo();
    let todo_state = use_list_todos_subscription();
    let mut new_title = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let mut adding = use_signal(|| false);

    let todo_items = todo_state.data.clone().unwrap_or_default();
    let remaining_count = todo_items.iter().filter(|t| !t.completed).count();

    let submit = {
        let create_todo = create_todo.clone();
        let signals = signals.clone();
        move || {
            let title = new_title().trim().to_string();
            if title.is_empty() || adding() {
                return;
            }
            error.set(None);
            adding.set(true);
            let create_todo = create_todo.clone();
            let signals = signals.clone();
            spawn(async move {
                match create_todo.call(CreateTodoInput::new(title.clone())).await {
                    Ok(_) => {
                        signals.track_with_properties("todo_created", json!({"title": &title}));
                        new_title.set(String::new());
                    }
                    Err(err) => {
                        signals.track_with_properties(
                            "todo_create_error",
                            json!({"error": &err.message}),
                        );
                        error.set(Some(err.message));
                    }
                }
                adding.set(false);
            });
        }
    };

    rsx! {
        section { class: "input-panel",
            div { class: "input-row",
                input {
                    r#type: "text",
                    placeholder: "What needs to be done?",
                    value: new_title(),
                    disabled: adding(),
                    oninput: move |event| new_title.set(event.value()),
                    onkeydown: {
                        let mut submit = submit.clone();
                        move |event: KeyboardEvent| {
                            if event.key().to_string() == "Enter" {
                                submit();
                            }
                        }
                    },
                }
                button {
                    disabled: adding() || new_title().trim().is_empty(),
                    onclick: {
                        let mut submit = submit.clone();
                        move |_| submit()
                    },
                    if adding() { "Adding..." } else { "Add" }
                }
            }
            if let Some(message) = error() {
                p { class: "error", "{message}" }
            }
        }
        section { class: "list-panel",
            if !todo_items.is_empty() {
                div { class: "list-head",
                    span { class: "summary", "{remaining_count} remaining" }
                }
            }
            if todo_state.loading {
                p { class: "status", "Loading..." }
            } else if let Some(todo_error) = todo_state.error.as_ref() {
                p { class: "error", "{todo_error.message}" }
            } else if todo_items.is_empty() {
                p { class: "status", "No todos yet. Add one above!" }
            } else {
                ul {
                    for todo in todo_items {
                        TodoItem {
                            key: "{todo.id}",
                            todo: todo,
                            error: error,
                        }
                    }
                }
                p { class: "count", "{remaining_count} remaining" }
            }
        }
    }
}
