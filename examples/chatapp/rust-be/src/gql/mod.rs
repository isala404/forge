//! The GraphQL surface: types, queries, mutations, subscriptions. async-graphql is
//! code-first; the SDL it emits (`Schema::sdl`) is asserted equal to the canonical
//! `schema.graphql` by a test. Every relational field resolves through a DataLoader.

mod auth;
mod chat;
mod helpers;
mod message;
mod ops;
mod presence;
mod receipt;
mod types;

use async_graphql::dataloader::DataLoader;
use async_graphql::{MergedObject, MergedSubscription};

use crate::context::Ctx;
use crate::loaders::AppLoader;

use auth::{AuthMutation, AuthQuery};
use chat::{ChatMutation, ChatQuery};
use message::{MessageMutation, MessageQuery, MessageSubscription};
use ops::{OpsMutation, OpsQuery};
use presence::{PresenceMutation, PresenceQuery, PresenceSubscription};
use receipt::{ReceiptMutation, ReceiptSubscription};

#[derive(MergedObject, Default)]
pub struct Query(AuthQuery, ChatQuery, MessageQuery, PresenceQuery, OpsQuery);

#[derive(MergedObject, Default)]
pub struct Mutation(
    AuthMutation,
    ChatMutation,
    MessageMutation,
    PresenceMutation,
    ReceiptMutation,
    OpsMutation,
);

#[derive(MergedSubscription, Default)]
pub struct Subscription(MessageSubscription, PresenceSubscription, ReceiptSubscription);

pub type AppSchema = async_graphql::Schema<Query, Mutation, Subscription>;

pub fn schema(ctx: Ctx) -> AppSchema {
    let loader = DataLoader::new(AppLoader { ctx: ctx.clone() }, tokio::spawn);
    async_graphql::Schema::build(Query::default(), Mutation::default(), Subscription::default())
        .data(ctx)
        .data(loader)
        .finish()
}

pub fn sdl() -> String {
    async_graphql::Schema::build(Query::default(), Mutation::default(), Subscription::default())
        .finish()
        .sdl()
}

#[cfg(test)]
mod tests {
    use super::helpers::{forge_error_code, validate_credentials};
    use forge::ForgeError;

    fn normalize(sdl: &str) -> Vec<String> {
        let mut blocks: Vec<String> = Vec::new();
        let mut current: Vec<String> = Vec::new();
        let mut depth = 0i32;
        for raw in sdl.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('"') {
                continue;
            }
            if depth == 0 && current.is_empty() && !looks_like_def(line) {
                continue;
            }
            current.push(line.to_string());
            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            if depth == 0 && !current.is_empty() {
                blocks.push(normalize_block(&current));
                current.clear();
            }
        }
        blocks.sort();
        blocks
    }

    fn looks_like_def(line: &str) -> bool {
        ["type ", "enum ", "scalar ", "input ", "interface ", "union "]
            .iter()
            .any(|k| line.starts_with(k))
    }

    fn normalize_block(lines: &[String]) -> String {
        if lines.len() <= 1 {
            return lines.join(" ");
        }
        let header = lines.first().cloned().unwrap_or_default();
        let mut body: Vec<String> = lines[1..lines.len() - 1].to_vec();
        body.sort();
        format!("{header}\n{}\n}}", body.join("\n"))
    }

    #[test]
    fn sdl_matches_canonical() {
        let emitted = normalize(&super::sdl());
        let canonical = normalize(include_str!("../../schema.graphql"));
        assert_eq!(
            emitted, canonical,
            "emitted SDL differs from canonical schema.graphql under normalized comparison"
        );
    }

    #[test]
    fn forge_errors_map_to_graphql_codes() {
        assert_eq!(forge_error_code(&ForgeError::NotFound), "NOT_FOUND");
        assert_eq!(forge_error_code(&ForgeError::Invalid("x".into())), "INVALID");
        assert_eq!(forge_error_code(&ForgeError::Limit("x".into())), "LIMIT");
        assert_eq!(forge_error_code(&ForgeError::Precondition("x".into())), "PRECONDITION");
        assert_eq!(forge_error_code(&ForgeError::Unavailable("x".into())), "UNAVAILABLE");
        assert_eq!(forge_error_code(&ForgeError::Config("x".into())), "CONFIG");
    }

    #[test]
    fn credential_validation() {
        assert!(validate_credentials("alice", "hunter2").is_ok());
        assert!(validate_credentials("al", "hunter2").is_err());
        assert!(validate_credentials("  ab  ", "hunter2").is_err());
        assert!(validate_credentials("alice", "short").is_err());
    }
}
