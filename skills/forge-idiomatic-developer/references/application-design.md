# Forge application design guidance

Read this reference when a task involves a nontrivial service, storage design,
cross-system state transition, authentication workflow, lifecycle, or test strategy.
Apply the sections relevant to the feature.

## Match storage to the domain

Choose storage from the operations and invariants the feature needs. Forge KV fits
bounded state read by exact key or predictable prefix, including TTLs, conditional
writes, counters, and compare-and-swap. Use a query-oriented datastore for joins,
secondary queries, relational constraints, multi-record transactions, search,
reporting, or unbounded collections. Avoid recreating database indexes or
transactions through unrelated KV keys.

For a substantial multi-primitive service, keep a state/ownership document using the
repository's existing convention. Record canonical locations, key/table patterns,
owners, retention, queues, topics, blobs, and consistency boundaries.

## Make cross-system work repairable when it needs to be

Use one datastore transaction for related records when the selected datastore
supports it. When a database write and a Forge/external side effect cannot share a
transaction, choose the mechanism from the consequence of loss or duplication:

- A post-commit enqueue/publish plus monitoring can be enough for best-effort work.
- Use a transactional outbox when losing the side effect after the domain commit is
  unacceptable.
- Use an inbox or unique idempotency record when applying a durable, non-idempotent
  consumer effect.
- Use compensation or a pending state when object storage and metadata can partially
  succeed.

When a pub/sub event represents durable state, publish after commit and make clients
reload it. Typing indicators, transient presence, and progress hints are intentionally
ephemeral and need no preceding durable write.

For blob uploads, either upload under an unguessable/inaccessible key and publish
metadata afterward, or create non-visible pending metadata and expose it only after
the upload. Compensate on failure when orphan cost matters. On deletion, hide/remove
user-visible metadata first and delete bytes afterward so a partial failure leaves an
invisible orphan rather than a dangling visible record. A separate metadata datastore
is needed only when Forge object metadata/key structure cannot express the ownership,
query, workflow, or retention requirements.

## Scope queue idempotency to the effect

A job can be redelivered after its handler applies an effect but before the ack lands,
and several workers can consume one queue.

- Use a domain operation/event id when multiple jobs can represent the same effect.
- Use the stable job id when redelivery of one job is the duplicate source.
- Derive a per-effect key for fan-out, such as `job-id:recipient-id`.
- Pass the same logical key to external providers that support idempotency.
- Ack only after the effect succeeds. Producer dedup reduces enqueues but does not
  make a consumer exactly-once.
- Heartbeat work that can exceed its visibility window.

The deployed system needs a consumer for each produced queue, but producer and worker
may live in different services or repositories.

## Design authentication flows explicitly

Forge auth hashes passwords and owns opaque sessions, API keys, and one-time tokens;
the application owns user profiles, roles, memberships, and authorization policy.
Those records may live in KV or another datastore according to the storage criteria
above.

- Validate presented session/API tokens server-side, then load the application user
  and authorization context.
- Run credential limiters before expensive password work. Choose fail-closed when a
  bypass creates unacceptable security/financial risk; choose fail-open, monitoring,
  and defense in depth when availability takes precedence.
- Trust forwarding headers only behind a configured proxy that overwrites them.
- Normalize identifiers consistently and use constant-shape invalid-credential paths
  where account discovery matters.
- Centralize authorization policy enough to prevent route-by-route drift, using the
  architecture that fits the project (policy layer, middleware, service, or loader).
- Parse request types, lengths, enums, and cross-field rules explicitly on create and
  update. Safe conversion from form strings is expected.

One-time token consumption is destructive: Forge has no peek, reservation, or
rollback operation. If consumption must be followed by a separate durable mutation,
design a claim/recovery protocol, compensation, or a user-visible retry path. Checking
a safely authenticated no-op first may avoid wasting a token, but must not bypass
token validation, authorization, or abuse controls.

For monetary or measured quantities, use an exact representation with an explicit
currency/asset and scale. Integer minor units work for conventional fixed-exponent
currencies; decimal or integer-plus-scale may fit other assets. Never aggregate
different currencies implicitly or use binary floating point for balances.

## Bound reads according to growth

Bound every potentially unbounded read. Paginate collections that can grow and choose
cursor versus offset from consistency and navigation needs. Add indexes from real
query patterns, expected scale, and query plans rather than requiring one mechanically
for every list.

Forge KV scans are weakly consistent and callers must tolerate duplicates across
pages. They are appropriate for bounded keyspaces and operational iteration; move to
a query-oriented store when the feature needs richer access patterns.

## Shut down according to process topology

Retain handles for managed loops. Stop new work where applicable, signal workers and
scheduler/maintenance loops, close subscriptions, and await their completion before
process exit. Web servers, workers, and producers may be separate processes, so apply
only the steps owned by the current process. Use a bounded outer timeout when the
platform requires one and log forced termination.

## Test proportionally to risk

Use real-binding integration tests for the Forge boundary and Postgres tests for
persistence, concurrency, restart, and transaction claims. Memory backends, mocks,
and fakes remain useful for isolated domain logic, deterministic failure injection,
and fast tests.

Relevant tests may include conditional-write races, queue redelivery, lease loss,
pagination, backend failure mode, token recovery, shutdown, authorization isolation,
or browser behavior. Select tests from the behavior being built; workers, libraries,
CLIs, and non-web APIs do not need browser-only checks.
