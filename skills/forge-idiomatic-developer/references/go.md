# Forge Go reference

The module is `github.com/isala404/forge/bindings/go`. It is a pure-Go implementation with native pgx access and no cgo or Rust toolchain. Methods take an application-owned `context.Context`; Forge installs no signal handlers. Check the resolved module source when an exact signature or behavior matters.

```go
import forge "github.com/isala404/forge/bindings/go"

client, err := forge.InitDefault(ctx) // reads ./forge.toml
if err != nil { return err }
```

Go does not download an embedded PostgreSQL server. Use a normal PostgreSQL profile, or set `[forge] mode = "memory"` explicitly for database-free development and tests. Memory mode is process-local and non-durable, and production rejects it unless `AllowMemoryInProd` is set deliberately.

## Key/value

Values are `[]byte`. Missing keys return `nil` without an error.

```go
written, err := client.KVSet(ctx, "user:42", body, forge.SetOptions{
    Mode: forge.SetIfAbsent,
})
if err != nil { return err }
if !written { return errAlreadyExists }

body, err = client.KVGet(ctx, "user:42")
page, err := client.KVScan(ctx, "user:", nil, 100)
```

`KVCompareAndSwap` returns `false` on a mismatch. `KVScan` is paginated and weakly consistent; follow `page.Cursor` until it is `nil`.

## Queue and workers

Raw queue payloads are `[]byte`. Settle with `job.Receipt`, never the stable job id.

```go
if _, err := client.Enqueue(ctx, "emails", payload, forge.EnqueueOptions{
    MaxAttempts: 5,
    DedupID: operationID,
}); err != nil { return err }

job, err := client.Dequeue(ctx, "emails", forge.DequeueOptions{
    Visibility: 30 * time.Second,
    Wait: 20 * time.Second,
})
if err != nil { return err }
if job != nil {
    if err := deliver(job.Payload); err != nil {
        return client.Nack(ctx, job.Receipt, forge.NackOptions{RetryIn: time.Second})
    }
    return client.Ack(ctx, job.Receipt)
}
```

Prefer `RunWorker` for an owning consumer. It leases, heartbeats, acknowledges on a nil handler result, and nacks when the handler returns an error. Do not ack manually inside a managed handler. Handlers must honor the context for bounded shutdown.

```go
err := client.RunWorker(ctx, "emails", func(ctx context.Context, job forge.Job) error {
    return deliver(ctx, job.Payload)
}, forge.WorkerOptions{
    Concurrency: 4,
    Visibility: 30 * time.Second,
    DrainDeadline: 10 * time.Second,
})
```

Keep queue side effects idempotent. The job id is useful for redelivery, but fan-out effects usually need a key such as `jobID:recipientID` or a stable domain operation id.

## Pub/sub

Pub/sub is at-most-once and reaches live subscribers only. Publishes accept valid UTF-8 payloads up to 7,000 bytes. Subscribe before publishing and close the subscription when its owner exits.

```go
subscription, err := client.Subscribe(ctx, "user.updated")
if err != nil { return err }
defer subscription.Close()

for {
    payload, err := subscription.Next(ctx)
    if err != nil { return err }
    if payload == nil { return nil }
    handle(payload)
}
```

Publish from another request or process after the subscription is ready:

```go
if err := client.Publish(ctx, "user.updated", body); err != nil { return err }
```

Reconnect by subscribing again and reconciling durable state. Do not use pub/sub as a durable log.

## Blob

Blob methods are bytes-native. The application owns authorization for every key and presigned request.

```go
err := client.BlobPut(ctx, key, body, forge.PutOptions{ContentType: "image/png"})
body, err := client.BlobGet(ctx, key)
info, err := client.BlobHead(ctx, key)
err = client.BlobDelete(ctx, key)
```

Proxy presigning requires a non-empty `[blob].signing_secret`. Treat signed URLs as bearer credentials and redact their query strings.

## Auth

Forge owns credential mechanics, sessions, API keys, and one-time tokens. The application still owns user records and authorization policy.

```go
hash, err := client.HashPassword(ctx, password)
ok, err := client.VerifyPassword(ctx, password, hash)
if ok && client.NeedsRehash(hash) { /* replace the hash during login */ }

token, err := client.CreateSession(ctx, userID, forge.SessionOptions{})
session, err := client.ValidateSession(ctx, token)
```

Plaintext session tokens, API-key secrets, and one-time tokens are shown once. Store only the user and authorization data your application owns.

## Rate limits, schedules, config, and flags

`RateLimitCheck` returns a decision, not an error, when a request is denied. Choose fail-open behavior from the consequence of bypassing the limit.

`ScheduleCron` and `ScheduleAt` enqueue work only when the application calls `RunSchedulerOnce`. Run scheduler ticks and `Maintain` on an interval when the selected features need them.

`ConfigGet`, `ConfigSet`, and `ConfigDelete` handle runtime values. `Flag` falls back to the caller default. Use `SetFlag` and `FlagDetails` for typed rollout rules and evaluation details.

## Lifecycle, errors, and tests

Cancel the application context, wait for owned workers and scheduler loops, then call `Close(ctx)` with a bounded shutdown context. Do not exit while cleanup is still running.

Forge returns `*forge.Error` with a stable code, retryable flag, operation, backend, safe message, and the original local cause through `errors.Unwrap`. Use `forge.ErrorCodeOf(err)` and `forge.IsRetryable(err)` instead of matching display text.

Use `NewMemoryForTesting` with `NewManualClock` and `NewSeededReader` for deterministic tests. Advance time with `AdvanceTestClock` instead of sleeping. Seeded entropy is for tests only.
