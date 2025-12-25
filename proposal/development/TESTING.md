# Testing

> *Confidence at every level*

---

## Overview

FORGE provides testing utilities at every level:

- **Unit tests** — Test individual functions in isolation
- **Integration tests** — Test functions against a real database
- **Cluster tests** — Test multi-node behavior
- **End-to-end tests** — Test full user flows via the API

---

## Testing Philosophy

1. **Use real PostgreSQL** — SQLite has different behavior; test against what you deploy
2. **Transactions for isolation** — Each test runs in a transaction that rolls back
3. **Fast by default** — Parallel tests, shared database, minimal setup
4. **Escape hatches exist** — When you need real commits, multi-connection tests, etc.

---

## Unit Tests

Test pure business logic without database:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_discount() {
        let price = Money::new(100, "USD");
        let discount = calculate_discount(&price, DiscountType::Percentage(10));

        assert_eq!(discount.amount, 10);
    }

    #[test]
    fn test_validate_email() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("invalid").is_err());
    }
}
```

---

## Integration Tests

### Basic Setup

```rust
use forge::testing::*;

#[tokio::test]
async fn test_create_and_fetch_project() {
    // Creates isolated test context with transaction
    let ctx = TestContext::new().await;

    // Create a user first
    let user = ctx.mutate(create_user, CreateUserInput {
        email: "test@example.com".into(),
        name: "Test User".into(),
    }).await.unwrap();

    // Create a project
    let project = ctx.mutate(create_project, CreateProjectInput {
        name: "My Project".into(),
        owner_id: user.id,
    }).await.unwrap();

    assert_eq!(project.name, "My Project");
    assert_eq!(project.owner_id, user.id);

    // Query it back
    let found = ctx.query(get_project, project.id).await.unwrap();
    assert_eq!(found.id, project.id);

    // Transaction rolls back automatically — no cleanup needed
}
```

### Test Context Features

```rust
#[tokio::test]
async fn test_context_features() {
    let ctx = TestContext::new()
        // Seed with specific user
        .with_user(User {
            id: uuid!("550e8400-e29b-41d4-a716-446655440000"),
            email: "known@example.com".into(),
            name: "Known User".into(),
        })
        // Set auth context
        .as_user(uuid!("550e8400-e29b-41d4-a716-446655440000"))
        // Enable debug logging
        .with_logging(true)
        .build()
        .await;

    // ctx.user_id is now set for auth checks
    let projects = ctx.query(get_my_projects, ()).await.unwrap();
}
```

### Testing Jobs

```rust
#[tokio::test]
async fn test_job_execution() {
    let ctx = TestContext::new().await;

    // Dispatch a job
    let job_id = ctx.dispatch(send_welcome_email, SendEmailInput {
        user_id: user.id,
        template: "welcome".into(),
    }).await.unwrap();

    // Job is queued but not executed yet
    assert!(ctx.job_status(job_id).await == JobStatus::Pending);

    // Run all pending jobs synchronously
    ctx.run_jobs().await;

    // Now it's completed
    assert!(ctx.job_status(job_id).await == JobStatus::Completed);

    // Check side effects (e.g., email was "sent")
    let emails = ctx.sent_emails().await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].to, "test@example.com");
}
```

### Testing Workflows

```rust
#[tokio::test]
async fn test_workflow_success() {
    let ctx = TestContext::new()
        // Mock external services
        .mock_http("https://api.stripe.com/*", |req| {
            json!({ "id": "cus_123", "email": req.body["email"] })
        })
        .build()
        .await;

    // Start workflow
    let handle = ctx.start_workflow(user_onboarding, OnboardingInput {
        email: "new@example.com".into(),
        name: "New User".into(),
    }).await.unwrap();

    // Run workflow to completion
    ctx.run_workflow(handle.id).await;

    // Check workflow completed
    assert_eq!(ctx.workflow_status(handle.id).await, WorkflowStatus::Completed);

    // Verify all steps ran
    assert!(ctx.workflow_step_completed(handle.id, "create_user").await);
    assert!(ctx.workflow_step_completed(handle.id, "setup_stripe").await);
}

#[tokio::test]
async fn test_workflow_compensation() {
    let ctx = TestContext::new()
        // Stripe fails on this test
        .mock_http("https://api.stripe.com/*", |_| {
            HttpError::status(500, "Internal error")
        })
        .build()
        .await;

    let handle = ctx.start_workflow(user_onboarding, OnboardingInput {
        email: "fail@example.com".into(),
        name: "Fail User".into(),
    }).await.unwrap();

    ctx.run_workflow(handle.id).await;

    // Workflow should have compensated
    assert_eq!(ctx.workflow_status(handle.id).await, WorkflowStatus::Compensated);

    // User should have been deleted (compensation ran)
    let user = ctx.query(get_user_by_email, "fail@example.com").await;
    assert!(user.is_none());
}
```

### Testing Subscriptions

```rust
#[tokio::test]
async fn test_subscription_updates() {
    let ctx = TestContext::new().await;

    let user = ctx.mutate(create_user, CreateUserInput { ... }).await.unwrap();

    // Start a subscription
    let mut subscription = ctx.subscribe(get_user_projects, user.id).await;

    // Initial result
    let initial = subscription.next().await.unwrap();
    assert_eq!(initial.len(), 0);

    // Create a project
    ctx.mutate(create_project, CreateProjectInput {
        owner_id: user.id,
        name: "New Project".into(),
    }).await.unwrap();

    // Subscription should update
    let updated = subscription.next().await.unwrap();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].name, "New Project");
}
```

---

## Cluster Integration Tests

Test multi-node behavior in isolation:

### Basic Cluster Test

```rust
use forge::testing::cluster::*;

#[tokio::test]
async fn test_multi_node_job_distribution() {
    // Spin up a 3-node test cluster
    let cluster = TestCluster::new()
        .nodes(3)
        .build()
        .await;

    // All nodes should be active
    assert_eq!(cluster.active_nodes().await, 3);

    // Dispatch 100 jobs
    for i in 0..100 {
        cluster.dispatch(process_item, ProcessInput { id: i }).await.unwrap();
    }

    // Run all jobs across the cluster
    cluster.run_jobs().await;

    // Verify jobs were distributed across nodes
    let distribution = cluster.job_distribution().await;

    // Each node should have processed some jobs (not all on one node)
    for (node_id, count) in &distribution {
        assert!(*count > 10, "Node {} only processed {} jobs", node_id, count);
    }
}
```

### Testing Node Failure

```rust
#[tokio::test]
async fn test_node_failure_recovery() {
    let cluster = TestCluster::new()
        .nodes(3)
        .build()
        .await;

    // Dispatch jobs
    for i in 0..50 {
        cluster.dispatch(slow_job, SlowInput { id: i, duration_ms: 100 }).await.unwrap();
    }

    // Start processing (non-blocking)
    cluster.start_processing().await;

    // Wait for some jobs to be claimed
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Kill node 1 (simulates crash)
    cluster.kill_node(1).await;

    // Wait for failure detection
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Cluster should detect dead node
    assert_eq!(cluster.active_nodes().await, 2);

    // Let remaining nodes finish
    cluster.wait_for_jobs().await;

    // All jobs should still complete (orphaned jobs reassigned)
    assert_eq!(cluster.completed_jobs().await, 50);
}
```

### Testing Leader Election

```rust
#[tokio::test]
async fn test_leader_election() {
    let cluster = TestCluster::new()
        .nodes(3)
        .build()
        .await;

    // One node should be scheduler leader
    let leader_id = cluster.scheduler_leader().await.unwrap();
    assert!(cluster.is_node_active(leader_id).await);

    // Kill the leader
    cluster.kill_node_by_id(leader_id).await;

    // Wait for re-election (< 15 seconds per spec)
    tokio::time::sleep(Duration::from_secs(5)).await;

    // New leader should be elected
    let new_leader_id = cluster.scheduler_leader().await.unwrap();
    assert_ne!(new_leader_id, leader_id);
    assert!(cluster.is_node_active(new_leader_id).await);
}
```

### Testing Cross-Node Subscriptions

```rust
#[tokio::test]
async fn test_cross_node_subscription() {
    let cluster = TestCluster::new()
        .nodes(2)
        .build()
        .await;

    let user = cluster.node(0).mutate(create_user, CreateUserInput { ... }).await.unwrap();

    // Subscribe on node 0
    let mut sub = cluster.node(0).subscribe(get_user, user.id).await;
    let initial = sub.next().await.unwrap();
    assert_eq!(initial.name, "Test User");

    // Mutate on node 1 (different node)
    cluster.node(1).mutate(update_user, UpdateUserInput {
        id: user.id,
        name: "Updated Name".into(),
    }).await.unwrap();

    // Subscription on node 0 should receive update
    let updated = sub.next_timeout(Duration::from_secs(2)).await.unwrap();
    assert_eq!(updated.name, "Updated Name");
}
```

### Testing Network Partitions

```rust
#[tokio::test]
async fn test_network_partition() {
    let cluster = TestCluster::new()
        .nodes(3)
        .build()
        .await;

    // Partition node 2 from the cluster
    cluster.partition_node(2).await;

    // Node 2 can't reach PostgreSQL
    assert!(!cluster.node(2).can_reach_db().await);

    // Node 2 should stop accepting requests
    let result = cluster.node(2).mutate(create_project, CreateProjectInput { ... }).await;
    assert!(result.is_err());

    // Nodes 0 and 1 continue operating
    let project = cluster.node(0).mutate(create_project, CreateProjectInput { ... }).await;
    assert!(project.is_ok());

    // Heal the partition
    cluster.heal_partition(2).await;

    // Wait for node to rejoin
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Node 2 should be active again
    assert_eq!(cluster.active_nodes().await, 3);
}
```

---

## Test Configuration

### Test Database

```toml
# forge.toml

[testing]
# Dedicated test database (recommended)
database_url = "postgres://forge:forge@localhost:5432/forge_test"

# Parallel test execution
parallel = true
max_connections = 50

# Test timeouts
default_timeout = "30s"
job_timeout = "10s"
workflow_timeout = "60s"
```

### CI Configuration

```yaml
# .github/workflows/test.yml

name: Tests
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest

    services:
      postgres:
        image: postgres:16
        env:
          POSTGRES_DB: forge_test
          POSTGRES_USER: forge
          POSTGRES_PASSWORD: forge
        ports:
          - 5432:5432
        options: >-
          --health-cmd pg_isready
          --health-interval 10s
          --health-timeout 5s
          --health-retries 5

    steps:
      - uses: actions/checkout@v4

      - name: Install Rust
        uses: dtolnay/rust-action@stable

      - name: Run migrations
        run: cargo run -- db migrate
        env:
          DATABASE_URL: postgres://forge:forge@localhost:5432/forge_test

      - name: Run tests
        run: cargo test
        env:
          DATABASE_URL: postgres://forge:forge@localhost:5432/forge_test
          RUST_BACKTRACE: 1

  cluster-tests:
    runs-on: ubuntu-latest

    services:
      postgres:
        image: postgres:16
        # ... same as above

    steps:
      - uses: actions/checkout@v4

      - name: Run cluster tests
        run: cargo test --test cluster_tests
        env:
          DATABASE_URL: postgres://forge:forge@localhost:5432/forge_test
          # Cluster tests need more time
          FORGE_TEST_TIMEOUT: 120s
```

---

## Performance Testing

### Load Testing Functions

```rust
use forge::testing::load::*;

#[tokio::test]
#[ignore] // Run explicitly: cargo test --ignored
async fn load_test_create_project() {
    let ctx = TestContext::new().await;

    let results = load_test()
        .concurrent_users(100)
        .duration(Duration::from_secs(30))
        .operation(|ctx| async move {
            ctx.mutate(create_project, CreateProjectInput {
                name: format!("Project-{}", Uuid::new_v4()),
                owner_id: ctx.user_id,
            }).await
        })
        .run(&ctx)
        .await;

    println!("Throughput: {:.2} req/sec", results.requests_per_second);
    println!("P50 latency: {:?}", results.latency_p50);
    println!("P99 latency: {:?}", results.latency_p99);
    println!("Error rate: {:.2}%", results.error_rate * 100.0);

    // Assert performance requirements
    assert!(results.requests_per_second > 500.0);
    assert!(results.latency_p99 < Duration::from_millis(100));
    assert!(results.error_rate < 0.01);
}
```

### Subscription Stress Test

```rust
#[tokio::test]
#[ignore]
async fn stress_test_subscriptions() {
    let ctx = TestContext::new().await;
    let user = ctx.create_user().await;

    // Create 1000 concurrent subscriptions
    let subscriptions: Vec<_> = (0..1000)
        .map(|_| ctx.subscribe(get_user_projects, user.id))
        .collect::<FuturesUnordered<_>>()
        .collect()
        .await;

    // Trigger updates
    for i in 0..100 {
        ctx.mutate(create_project, CreateProjectInput {
            name: format!("Project {}", i),
            owner_id: user.id,
        }).await.unwrap();
    }

    // All subscriptions should receive all updates
    for mut sub in subscriptions {
        let latest = sub.latest().await;
        assert_eq!(latest.len(), 100);
    }
}
```

---

## Mocking External Services

```rust
#[tokio::test]
async fn test_with_mocked_stripe() {
    let ctx = TestContext::new()
        .mock_http("https://api.stripe.com/v1/customers", |req| {
            assert_eq!(req.method, "POST");
            json!({
                "id": "cus_test_123",
                "email": req.body["email"],
                "created": 1234567890
            })
        })
        .mock_http("https://api.stripe.com/v1/customers/*", |req| {
            let customer_id = req.path.split('/').last().unwrap();
            json!({
                "id": customer_id,
                "email": "test@example.com"
            })
        })
        .build()
        .await;

    // Code that calls Stripe will hit the mock
    let result = ctx.mutate(create_stripe_customer, CreateCustomerInput {
        email: "test@example.com".into(),
    }).await.unwrap();

    assert_eq!(result.stripe_id, "cus_test_123");
}
```

---

## Test Utilities

### Factories

```rust
// tests/factories.rs

use forge::testing::*;

pub async fn create_test_user(ctx: &TestContext) -> User {
    ctx.mutate(create_user, CreateUserInput {
        email: format!("user-{}@example.com", Uuid::new_v4()),
        name: "Test User".into(),
    }).await.unwrap()
}

pub async fn create_test_project(ctx: &TestContext, owner: &User) -> Project {
    ctx.mutate(create_project, CreateProjectInput {
        name: format!("Project-{}", Uuid::new_v4()),
        owner_id: owner.id,
    }).await.unwrap()
}

// Usage
#[tokio::test]
async fn test_with_factories() {
    let ctx = TestContext::new().await;
    let user = create_test_user(&ctx).await;
    let project = create_test_project(&ctx, &user).await;

    // ...
}
```

### Assertions

```rust
use forge::testing::assertions::*;

#[tokio::test]
async fn test_with_assertions() {
    let ctx = TestContext::new().await;

    // Assert query returns expected count
    assert_query_count!(ctx, get_projects_by_owner, owner_id, 0);

    ctx.mutate(create_project, ...).await.unwrap();

    assert_query_count!(ctx, get_projects_by_owner, owner_id, 1);

    // Assert mutation fails with specific error
    assert_mutation_fails!(
        ctx,
        create_project,
        CreateProjectInput { name: "".into(), .. },
        "name cannot be empty"
    );

    // Assert job was dispatched
    assert_job_dispatched!(ctx, send_email, |input| {
        input.to == "test@example.com"
    });
}
```

---

## Best Practices

### 1. One Assertion Per Test (When Possible)

```rust
// Good: Clear what's being tested
#[tokio::test]
async fn test_create_project_sets_status_to_draft() {
    let ctx = TestContext::new().await;
    let project = ctx.mutate(create_project, ...).await.unwrap();
    assert_eq!(project.status, ProjectStatus::Draft);
}

// Avoid: Multiple unrelated assertions
#[tokio::test]
async fn test_create_project() {
    let ctx = TestContext::new().await;
    let project = ctx.mutate(create_project, ...).await.unwrap();
    assert_eq!(project.status, ProjectStatus::Draft);
    assert!(project.created_at < Utc::now());
    assert_eq!(project.owner_id, owner.id);
    // What exactly are we testing?
}
```

### 2. Test Edge Cases

```rust
#[tokio::test]
async fn test_create_project_with_empty_name_fails() {
    let ctx = TestContext::new().await;
    let result = ctx.mutate(create_project, CreateProjectInput {
        name: "".into(),
        ..
    }).await;

    assert!(matches!(result, Err(ForgeError::Validation(_))));
}

#[tokio::test]
async fn test_create_project_with_duplicate_name_fails() {
    let ctx = TestContext::new().await;
    ctx.mutate(create_project, CreateProjectInput { name: "Existing".into(), .. }).await.unwrap();

    let result = ctx.mutate(create_project, CreateProjectInput { name: "Existing".into(), .. }).await;
    assert!(matches!(result, Err(ForgeError::Conflict(_))));
}
```

### 3. Use Descriptive Test Names

```rust
// Good
#[tokio::test]
async fn test_deleting_project_also_deletes_associated_tasks() { }

#[tokio::test]
async fn test_user_cannot_access_projects_they_do_not_own() { }

// Avoid
#[tokio::test]
async fn test_delete() { }

#[tokio::test]
async fn test_auth() { }
```

### 4. Isolate External Dependencies

```rust
// Production code
pub async fn create_customer(ctx: &Context, email: &str) -> Result<Customer> {
    let stripe_customer = ctx.stripe.create_customer(email).await?;
    // ...
}

// Test with mock
#[tokio::test]
async fn test_create_customer() {
    let ctx = TestContext::new()
        .mock_stripe(MockStripe::new()
            .on_create_customer(|email| Ok(StripeCustomer { id: "cus_123".into(), email })))
        .build()
        .await;

    let customer = ctx.mutate(create_customer, "test@example.com").await.unwrap();
    assert_eq!(customer.stripe_id, "cus_123");
}
```

---

## Related Documentation

- [Development](DEVELOPMENT.md) — Local development setup
- [Jobs](../core/JOBS.md) — Background job testing
- [Workflows](../core/WORKFLOWS.md) — Workflow testing
- [Clustering](../cluster/CLUSTERING.md) — Multi-node behavior
