mod routes;
mod types;
mod util;

use std::time::Duration;

use forgelib::Forge;
use rocket::{Build, Rocket};

use crate::routes::AppState;
use crate::util::env_or;

async fn maintenance(forge: Forge) {
    loop {
        if let Err(err) = forge.run_scheduler_once().await {
            tracing::warn!(error = %err, "scheduler tick failed");
        } else if let Ok(diagnostics) = forge.schedule().diagnostics().await
            && diagnostics.due_count > 0
        {
            tracing::warn!(
                due = diagnostics.due_count,
                lag_ms = diagnostics.lag.map(|lag| lag.as_millis()),
                "scheduler remains behind"
            );
        }
        if let Err(err) = forge.maintain().await {
            tracing::warn!(error = %err, "maintenance sweep failed");
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

fn app(forge: Forge, bind: String, port: u16) -> Rocket<Build> {
    let figment = rocket::Config::figment()
        .merge(("address", bind))
        .merge(("port", port));

    routes::mount_routes(rocket::custom(figment).manage(AppState { forge }))
}

#[rocket::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    if std::env::args().any(|arg| arg == "--migrate") {
        let reports = Forge::migrate().await?;
        for report in &reports {
            eprintln!(
                "{}: {} ({})",
                report.target,
                report.state.as_str(),
                report.message
            );
        }
        anyhow::ensure!(
            reports.iter().all(|report| report.is_compatible()),
            "migration failed"
        );
        return Ok(());
    }

    // Reads ./forge.toml (the connection string and any ${VAR} references live there).
    let forge = Forge::init().await?;
    tokio::spawn(maintenance(forge.clone()));

    let port = env_or("PORT", "9081").parse::<u16>()?;
    let bind = env_or("BIND", "127.0.0.1");
    tracing::info!("todoapp-rust-be listening on http://{bind}:{port}");
    app(forge.clone(), bind, port).launch().await?;
    forge.close(Duration::from_secs(30)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use forgelib::Forge;
    use rocket::http::{ContentType, Header, Status};
    use rocket::local::asynchronous::Client;
    use serde_json::{Value, json};
    use std::time::Duration;

    const MEMORY: &str = "[forge]\nmode = \"memory\"\nenvironment = \"test\"\n";

    async fn client() -> (Client, Forge) {
        let forge = Forge::init_from_str(MEMORY).await.unwrap();
        let client = Client::tracked(super::app(forge.clone(), "127.0.0.1".to_string(), 0))
            .await
            .unwrap();
        (client, forge)
    }

    async fn signup(client: &Client, email: &str) -> (String, String) {
        let response = client
            .post("/api/signup")
            .header(ContentType::JSON)
            .body(json!({"email": email, "password": "correct horse"}).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Created);
        let body = response.into_json::<Value>().await.unwrap();
        (
            body["token"].as_str().unwrap().to_string(),
            body["user"]["id"].as_str().unwrap().to_string(),
        )
    }

    fn bearer(token: &str) -> Header<'static> {
        Header::new("Authorization", format!("Bearer {token}"))
    }

    #[rocket::async_test]
    async fn health_meta_and_invalid_requests_are_bounded() {
        let (client, forge) = client().await;

        let health = client.get("/healthz").dispatch().await;
        assert_eq!(health.status(), Status::Ok);
        assert_eq!(health.into_string().await.as_deref(), Some("ok"));

        let meta = client.get("/api/meta").dispatch().await;
        assert_eq!(meta.status(), Status::Ok);
        let meta = meta.into_json::<Value>().await.unwrap();
        assert_eq!(meta["backend"], "rust");
        assert_eq!(meta["forge"].as_array().unwrap().len(), 8);

        let invalid = client
            .post("/api/signup")
            .header(ContentType::JSON)
            .body(json!({"email": "bad", "password": "short"}).to_string())
            .dispatch()
            .await;
        assert_eq!(invalid.status(), Status::BadRequest);

        let missing = client
            .get("/api/me")
            .header(bearer("missing"))
            .dispatch()
            .await;
        assert_eq!(missing.status(), Status::Unauthorized);

        forge.close(Duration::from_secs(1)).await.unwrap();
    }

    #[rocket::async_test]
    async fn auth_and_todos_enforce_duplicates_isolation_and_logout() {
        let (client, forge) = client().await;
        let (first_token, _) = signup(&client, "first@example.com").await;
        let (second_token, _) = signup(&client, "second@example.com").await;

        let duplicate = client
            .post("/api/signup")
            .header(ContentType::JSON)
            .body(json!({"email": "FIRST@example.com", "password": "correct horse"}).to_string())
            .dispatch()
            .await;
        assert_eq!(duplicate.status(), Status::Conflict);

        let bad_login = client
            .post("/api/login")
            .header(ContentType::JSON)
            .body(json!({"email": "first@example.com", "password": "wrong password"}).to_string())
            .dispatch()
            .await;
        assert_eq!(bad_login.status(), Status::Unauthorized);

        let invalid_todo = client
            .post("/api/todos")
            .header(ContentType::JSON)
            .header(bearer(&first_token))
            .body(json!({"title": "  "}).to_string())
            .dispatch()
            .await;
        assert_eq!(invalid_todo.status(), Status::BadRequest);

        let created = client
            .post("/api/todos")
            .header(ContentType::JSON)
            .header(bearer(&first_token))
            .body(json!({"title": "Ship backend"}).to_string())
            .dispatch()
            .await;
        assert_eq!(created.status(), Status::Created);
        let created = created.into_json::<Value>().await.unwrap();
        let todo_id = created["id"].as_str().unwrap();

        let other_list = client
            .get("/api/todos")
            .header(bearer(&second_token))
            .dispatch()
            .await
            .into_json::<Value>()
            .await
            .unwrap();
        assert!(other_list["todos"].as_array().unwrap().is_empty());

        let updated = client
            .patch(format!("/api/todos/{todo_id}"))
            .header(ContentType::JSON)
            .header(bearer(&first_token))
            .body(json!({"completed": true}).to_string())
            .dispatch()
            .await;
        assert_eq!(updated.status(), Status::Ok);
        assert_eq!(
            updated.into_json::<Value>().await.unwrap()["completed"],
            true
        );

        let deleted = client
            .delete(format!("/api/todos/{todo_id}"))
            .header(bearer(&first_token))
            .dispatch()
            .await;
        assert_eq!(deleted.status(), Status::NoContent);
        let missing = client
            .delete(format!("/api/todos/{todo_id}"))
            .header(bearer(&first_token))
            .dispatch()
            .await;
        assert_eq!(missing.status(), Status::NotFound);

        let meta = client
            .get("/api/meta")
            .dispatch()
            .await
            .into_json::<Value>()
            .await
            .unwrap();
        assert_eq!(meta["auditDepth"]["visible"], 3);

        let logout = client
            .post("/api/logout")
            .header(bearer(&first_token))
            .dispatch()
            .await;
        assert_eq!(logout.status(), Status::NoContent);
        let me = client
            .get("/api/me")
            .header(bearer(&first_token))
            .dispatch()
            .await;
        assert_eq!(me.status(), Status::Unauthorized);

        forge.close(Duration::from_secs(1)).await.unwrap();
    }
}
