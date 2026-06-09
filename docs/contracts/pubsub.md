# `pubsub` contract

Lineage: Postgres `LISTEN`/`NOTIFY`, with Redis pub/sub delivery semantics. The
realtime transport behind GraphQL subscriptions, live presence, and typing
indicators. **Not a queue** — no durability, no redelivery, no cross-connection
ordering. When a message must not be lost, use `queue`.

Facade: `forge.pubsub()` → `&dyn Pubsub`.

## Surface

```rust
async fn publish(&self, topic: &str, payload: Bytes) -> Result<()>;
async fn subscribe(&self, topic: &str) -> Result<Subscription>; // Subscription = Stream<Item = Result<Bytes>>
```

## Semantics

| Behavior | Guarantee |
| --- | --- |
| Delivery | **At-most-once, connected-only.** A payload reaches exactly the subscriptions that are established at publish time. A subscriber that connects later, or is disconnected when `publish` runs, never receives it. |
| Persistence | None. Nothing is stored; there is no backlog, replay, or redelivery. |
| Ordering | Per publishing connection, messages arrive in publish order. No total order across publishers. |
| Fan-out | One `publish` reaches all current subscribers of the topic. Zero subscribers is success, not an error. |
| Topic → channel | Topics are arbitrary UTF-8 (≤ `MAX_TOPIC_BYTES` = 256 B). Internally each maps to a `forge_<sha256[..32]>` Postgres channel, so any topic string is valid and distinct topics do not collide. |
| Payload | UTF-8 text, ≤ `MAX_PAYLOAD_BYTES` = 7000 B (Postgres caps NOTIFY at 8000 B). For larger data, publish a reference (e.g. a row id) and have the subscriber read the row. |
| Subscription lifetime | Each `subscribe` holds a dedicated Postgres connection until the stream is dropped. Dropping the stream unsubscribes and releases the connection. The stream ends (`None`) if the connection drops. |

## Errors

| Condition | Variant |
| --- | --- |
| empty topic, topic > 256 B | `Invalid` |
| non-UTF-8 payload | `Invalid` |
| payload > 7000 B | `Limit` |
| backend unavailable (publish), connect failure (subscribe) | `Unavailable` / `Backend` per the shared sqlx classification |

`publish` never reports "no subscribers" — that is a normal `Ok(())`.

## Observability

`publish` runs in span `forge.pubsub.publish` (fields: `pubsub.topic_hash`,
`pubsub.payload_bytes`, `outcome`, `error.variant`) and emits the standard
`forge_ops_total` / `forge_op_duration_seconds` / `forge_errors_total`. `subscribe`
emits a `forge_ops_total{op="subscribe"}` counter when a subscription is
established (it returns a long-lived stream, not a single completing op).

## Non-goals

Durability, replay, queue semantics, message acknowledgement, or wildcard/pattern
topics. Those belong to `queue` or a dedicated broker behind the same trait later.
