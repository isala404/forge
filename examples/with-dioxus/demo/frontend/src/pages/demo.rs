use dioxus::prelude::*;

use crate::API_URL;
use crate::components::{
    AuthCard, CacheCard, ExportCard, IssCard, McpCard, TradesCard, UsersSection, VerificationCard,
    WebhookCard,
};
use crate::forge::User;

#[component]
pub fn DemoPage() -> Element {
    let selected_user = use_signal(|| None::<User>);

    rsx! {
        main { class: "shell",
            div { class: "columns",
                div { class: "col",
                    IssCard {}
                    CacheCard {}
                    ExportCard {}
                    McpCard { api_url: API_URL.to_string() }
                }
                div { class: "col",
                    TradesCard {}
                    AuthCard {}
                    WebhookCard {}
                    VerificationCard { selected_user }
                }
            }

            UsersSection { selected_user }
        }
    }
}
