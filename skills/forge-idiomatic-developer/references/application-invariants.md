# Forge — application invariants

Read this reference when building or reviewing a complete service. The language
references tell you how to call Forge; this file covers the state transitions and
lifecycle rules that keep individually correct calls from becoming an incorrect app.

## Write the state model first — and keep it true

List the canonical record, every secondary index, queue, topic, and retention rule
before writing handlers, and derive the layout from access paths: every list/read
endpoint must map to one tenant-bounded prefix or a multi-get (see the tables in
SKILL.md). A useful layout makes ownership and bounded access obvious:

```text
user:{user_id}                         -> canonical user
user-by-email:{normalized_email}       -> user_id index
activity:{user_id}:{event_or_job_id}   -> one immutable event, with TTL
```

The state-model doc is part of every diff that changes behavior. Before finishing,
reread it against the store layer; a doc that says "single-use" while the code allows
five uses is a bug in one of them.

One conditional KV write is atomic. A sequence of writes to different keys is not.
For every multi-key transition:

1. Pick the canonical record and define how readers handle a missing index or target.
2. Choose primary-first or index-first deliberately. Neither is universally safer:
   index-first can strand a reservation that blocks a valid retry; primary-first can
   strand a canonical record with no index. Choose the orphan that readers can reject
   and reconciliation can repair.
3. Handle a conditional-write loser and compensate every later failure. Never claim
   that separate KV calls form a transaction; on an indexed read, verify the
   canonical target still exists.
4. Store an owner/id in a reservation so cleanup is attributable.
5. Use the app's SQL database and a real transaction when no partial-state window is
   acceptable.

**Deletes run in reverse creation order**: remove the index/metadata first, the
referenced payload (blob, canonical record) last, so a crash in between leaves an
invisible orphan rather than a dangling reference users can hit.

## Treat queues as at-least-once and concurrent

A job may be redelivered after its handler committed a side effect but before the ack
landed, and several workers may consume the same queue. Producer dedup reduces
duplicate enqueues; it does not make consumers exactly-once.

Make side effects idempotent using the stable job id:

- A Forge-record side effect gets a deterministic key created conditionally — one key
  per event, never get/mutate/set of a shared JSON array (this is the same
  write-shape rule from SKILL.md; it applies doubly under redelivery).
- An external API call passes the job id as the provider's idempotency key.
- A side effect plus idempotency marker in app SQL commit in one transaction; use an
  outbox/inbox when work crosses systems.

Ack only after the durable side effect succeeds; throw to let the managed worker nack
and retry. Let poison jobs reach the DLQ instead of acking incomplete work.

## Make auth boundaries fail safely

- Normalize identifiers (email) before rate-limit and uniqueness keys.
- Rate-limit before password hashing or verification. Credential, reset, invite, and
  API-key paths fail closed: a limiter outage must not remove the brute-force
  control. Key IP-based limits on the **socket address** unless a trusted proxy is
  explicitly configured — `x-forwarded-for` is client-controlled in direct-serve
  deployments and makes the limiter a suggestion.
- Check no-op conditions **before** consuming anything single-use: an
  already-member accepting an invite should not burn one of its uses; a duplicate
  request should not spend limiter budget it doesn't need.
- One authorization gate per resource type, taking a minimum role, called from the
  resource loader. Hand-rolled role comparisons per route drift from each other and
  from their own error messages. The loader fetches only what its routes need.
- Validate JSON types, lengths, and allowed values at the transport boundary — no
  coercion. Cross-field invariants (end date after start date, shares summing to the
  total) must hold on **every** write path; an update that revalidates fields
  individually but skips the cross-field check reintroduces states create forbids.
- Money is integer minor units, never floats, and never aggregated across currencies
  without grouping.
- Validate every presented session token server-side, then load the canonical user;
  possession of local state alone is not authentication.
- Exercise every partial-failure edge in signup and account changes: a uniqueness
  index must not point forever at a user that was never created, and a losing record
  must not remain reachable as a valid account.

## Keep scans bounded — and degrading

The simple scan call returns only its first page. Use the paginated scan, follow
cursors to completion, and multi-get page values instead of one read per key.

Partition prefixes by tenant or owner; give event-like data TTL retention. A size cap
on user-generated data **truncates and returns a cursor — it never throws**: hitting
a cap is normal growth, not an error, and a throw turns a 501st record into a
permanent outage for that endpoint. If the store layer paginates, the HTTP layer in
front of it must expose the cursor too; returning "everything" just moves the
unbounded read up a layer.

Do not use a global KV scan as a request-time query engine; use app SQL when the
product needs arbitrary filtering, joins, or durable pagination.

## Drain before process exit

Keep the promise/task/future returned by every managed loop. A stop signal asks a
worker to drain; it is not proof the drain finished.

1. Stop accepting new HTTP work.
2. Signal workers and scheduler/maintenance loops.
3. Await worker drain; close pub/sub subscriptions.
4. Await the HTTP server close, then exit.

A bounded outer timeout is fine if the platform requires one — log the forced path
and make it longer than the worker grace period. An immediate exit after abort
defeats graceful shutdown.

## Validate the invariants

Use the real binding, never a mock. Fast tests may select memory backends, but run
persistence/concurrency paths against embedded or scratch Postgres too. Unique
fixtures per run; run the suite twice so persistent limiter/dedup/KV state cannot
hide cleanup bugs.

Prove at minimum:

- two simultaneous uniqueness claims produce one winner and no valid orphan;
- the same queue job handled twice produces one logical side effect;
- two concurrent writers cannot lose each other's records (votes, comments, audit);
- every scan reads beyond the first page, respects tenant boundaries, and degrades
  (not throws) at its cap;
- limiter backend failure denies security-sensitive authentication;
- shutdown during an in-flight job waits for it or reaches the documented timeout;
- pure computation (splits, balances, date math, aggregation) has direct unit
  tests — an e2e flow will not catch a wrong number that still renders.

For a web client, add a real-browser smoke test: signup/login, logout/login,
hard-reload session restoration, multi-user isolation, invalid payload types,
eventual queue results, and console/request/5xx failures.
