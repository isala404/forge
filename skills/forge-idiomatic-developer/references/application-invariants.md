# Forge — application invariants

Read this reference when building or reviewing a complete service. The language
references tell you how to call Forge; this file covers the state transitions and
lifecycle rules that keep individually correct calls from becoming an incorrect app.

## Write the state model first

List the canonical record, every secondary index, queue, topic, and retention rule
before writing handlers. A useful layout makes ownership and bounded access obvious:

```text
user:{user_id}                         -> canonical user
user-by-email:{normalized_email}       -> user_id index
activity:{user_id}:{event_or_job_id}   -> one immutable event, with TTL
```

One conditional KV write is atomic. A sequence of writes to different keys is not.
For every multi-key transition:

1. Pick the canonical record and define how readers handle a missing index or target.
2. Choose primary-first or index-first deliberately.
3. Handle a conditional-write loser and compensate every later failure.
4. Store an owner/id in a reservation so cleanup and reconciliation are attributable.
5. Use the app's SQL database and a real transaction when crash-consistent atomicity
   across records is a requirement rather than a convenience.

Neither ordering is universally safer. Index-first can strand a reservation that
blocks a valid retry if the process dies before writing the primary; primary-first can
strand a canonical record if it dies before claiming the index. Choose the orphan
that readers can reject and reconciliation can repair, or use a transaction when no
partial-state window is acceptable.

For example, signup may create a user and a normalized-email index in either order,
but it must delete or reconcile the orphan created when the second step fails. Never
claim that separate KV calls form a transaction. On an indexed read, verify that the
canonical target still exists and fail safely if it does not.

## Treat queues as at-least-once and concurrent

A job may be delivered again after its handler committed a side effect but before its
ack reached Forge. The same queue can also be consumed by several worker processes or
several concurrency slots. Producer deduplication reduces duplicate enqueues; it does
not make the consumer exactly-once.

Make the side effect idempotent using the stable job id:

- If the side effect is a Forge record, make that record's key deterministic and
  create it conditionally. An activity feed should normally use one key per event,
  not get/mutate/set a shared JSON array that loses concurrent updates.
- If the side effect is an external API, pass the job id as the provider's
  idempotency key when supported.
- If the side effect and idempotency marker live in app SQL, commit them in one
  transaction. Use an outbox/inbox pattern when work crosses systems.

Ack only after the durable side effect succeeds. Throw/return an error to let the
managed worker nack and retry. Design poison jobs to reach the DLQ instead of catching
errors and acknowledging incomplete work.

## Make auth boundaries fail safely

- Normalize identifiers such as email before rate-limit and uniqueness keys.
- Run the rate limit before password hashing or verification. Credential, password
  reset, invite, and API-key verification endpoints should fail closed: a limiter
  outage must not silently remove the brute-force control. Reserve fail-open for
  low-risk paths where availability is intentionally more important than enforcement.
- Validate JSON types, lengths, and allowed values at the transport boundary. Do not
  turn arbitrary client input into valid-looking data with `Boolean(value)`,
  `String(value)`, or equivalent coercion.
- Validate every presented session token server-side and then load its canonical
  user. A browser restoring a stored token after a hard reload should remain in a
  loading state until that validation succeeds; possession of local state alone is
  not authentication.
- Exercise every partial-failure edge in signup and account changes. A uniqueness
  index must not point forever at a user that was never created, and a losing user
  record must not remain reachable as a valid account.

## Keep scans bounded

The simple scan call returns only its first page. Use the binding's paginated scan,
follow cursors until completion, and multi-get page values rather than issuing one
read per key.

Partition prefixes by tenant or owner and put TTL retention on event-like data. Cap
the response after sorting/filtering. Do not use an unbounded global KV scan as a
request-time query engine; use app SQL when the product needs arbitrary filtering,
ordering, joins, or durable pagination.

## Drain before process exit

Keep the promise, task, or future returned by every managed loop. A stop signal asks a
worker to stop leasing new jobs and drain in-flight work; it is not proof that the
work has finished.

Use this shutdown order:

1. Stop accepting new HTTP work.
2. Signal workers and scheduler/maintenance loops to stop.
3. Await worker drain and close pub/sub subscriptions.
4. Await the HTTP server close, then allow the process to exit.

Use a bounded outer timeout if the deployment platform requires one, but log the
forced path and make it longer than the worker grace period. An immediate
`process.exit()` after abort defeats graceful shutdown even though Forge has no client
`close()` method.

## Validate the invariants

Use the real binding rather than mocking it. Fast tests may select memory backends,
but run persistence/concurrency paths against scratch or embedded Postgres too. Make
fixtures unique per run and run the suite twice so persistent limiter, dedup, and KV
state cannot hide cleanup bugs.

At minimum, prove the invariants the application depends on:

- two simultaneous uniqueness claims produce one winner and no valid orphan;
- the same queue job handled twice produces one logical side effect;
- two workers cannot lose each other's activity/audit records;
- every scan reads beyond the first page and respects tenant boundaries;
- limiter backend failure denies security-sensitive authentication;
- a shutdown during an in-flight job waits for that job or reaches the documented
  bounded-timeout path.

For a web client, add a real-browser smoke test covering signup/login, logout/login,
hard-reload session restoration, multi-user isolation, invalid payload types, eventual
queue results, and console, request, or 5xx failures.
