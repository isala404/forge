# config — lineage: 12-factor env precedence + OpenFeature evaluation API

Runtime configuration and boolean feature flags.

## Lineage

Two well-worn designs, fused:

- **12-factor config.** Resolution is an explicit precedence chain — an env var
  (`FORGE_CFG_<KEY>`) overrides the stored value, which overrides the code default.
  Config that varies between deploys lives in the environment; the store is the
  shared default; the code default is the floor.
- **OpenFeature evaluation API.** `flag()` mirrors `getBooleanValue(key, default, ctx)`:
  it takes an `EvaluationContext`, and — the load-bearing inheritance — **it never
  errors and never panics.** Any failure (backend down, missing flag, malformed rule)
  resolves to the caller's `default`, with the reason logged via obs. A flag lookup can
  never take the app down.

The default backend is Postgres; the contract is the lowest common denominator a
Postgres store and an OpenFeature-style provider can both honor.

> **Flags fail to default, always.** `flag()` returns `bool`, not `Result`. When in
> doubt it returns what you passed as `default`. Design rollouts so "everyone gets the
> default" is a safe state, never an outage.

## Trait (Rust sketch — directional; this doc wins on conflict)

The trait is `ConfigStore` (the module is `config_store`), so it never collides with
`ForgeConfig` (the facade's init config). The facade accessor is `forge.config()`.

```rust
#[async_trait]
pub trait ConfigStore: Send + Sync {
    /// Resolved string value: env `FORGE_CFG_<KEY>` over store over `None`.
    /// `None` if unset at every layer. Cached in-process (30s TTL).
    async fn get_raw(&self, key: &str) -> Result<Option<String>>;

    /// Upsert the stored value. Visible everywhere within the 30s cache bound.
    /// An env override still shadows it.
    async fn set_raw(&self, key: &str, value: &str) -> Result<()>;

    /// OpenFeature getBooleanValue. NEVER errors, NEVER panics. Any failure
    /// resolves to `default`; the reason is logged via obs. See Resolution.
    async fn flag(&self, key: &str, default: bool, ctx: &EvalCtx) -> bool;

    /// Upsert a flag's rule. Visible everywhere within the 30s cache bound.
    async fn set_flag(&self, key: &str, rule: FlagRule) -> Result<()>;
}

/// Typed accessor, via an extension trait over the dyn-safe `ConfigStore` core.
#[async_trait]
pub trait ConfigExt: ConfigStore {
    /// Deserialize the resolved raw string. `None` if unset; `Invalid` if the
    /// stored value does not deserialize into `T`.
    async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>>;
}

/// OpenFeature EvaluationContext. `targeting_key` is the user/org id used for
/// stable percentage bucketing and allow-list matching.
#[non_exhaustive]
pub struct EvalCtx {
    pub targeting_key: Option<String>,
    pub attributes: std::collections::BTreeMap<String, String>,
}

/// Boolean flag rule. v1 is boolean-only (see Non-goals).
#[non_exhaustive]
pub enum FlagRule {
    On,                  // always true
    Off,                 // always false
    Percent(u8),         // in if stable_bucket(key, targeting_key) < p, p in 0..=100
    AllowList(Vec<String>), // in if targeting_key is listed
}
```

`get_raw`/`set_raw` are dyn-safe so `ConfigStore` can live behind `dyn`; `get::<T>` is an
extension because generic methods are not dyn-safe. `EvalCtx::attributes` is reserved
for future rule kinds; v1 rules read only `targeting_key`.

## Semantics

| op | behavior |
|----|----------|
| `get_raw` | Returns the resolved value following the precedence chain (see Resolution). `Some` from the first layer that has it; `None` if unset everywhere. Served from the in-process cache (≤30s stale). |
| `set_raw` | Upserts the **stored** value (last-write-wins). Returns once committed. The new value is visible to every `get_raw`/`get`/`flag` within 30s. An active `FORGE_CFG_<KEY>` env var still shadows it — `set_raw` does not change env. |
| `get::<T>` | Resolves the raw string as `get_raw`, then deserializes into `T`. `None` if the key is unset; a present value that fails to deserialize is `Invalid` (caller-side type mismatch). |
| `flag` | Resolves the flag against `ctx` and returns `bool`. **Never errors, never panics.** Missing flag, backend failure, or malformed rule → returns `default`, reason logged. A present rule is evaluated per Resolution. |
| `set_flag` | Upserts the flag's `FlagRule` (last-write-wins). Returns once committed; visible to `flag()` within 30s. |

`get_raw` and `flag` are separate keyspaces conceptually but share the 30s cache
contract: a `set_raw` or `set_flag` is observable everywhere within the staleness bound.

## Delivery / consistency guarantees

- **Last-write-wins.** Concurrent `set_raw`/`set_flag` on one key resolve to whichever
  commits last; no merge.
- **Bounded staleness, not strong consistency.** Values are cached in-process with a
  **30s TTL**. The staleness bound is part of the contract: a committed `set_raw`/
  `set_flag` is visible at every reader **within 30s**, and the cache may serve the
  prior value until then. There is no real-time push or invalidation (see Non-goals).
- **Read-your-writes is not guaranteed across instances**, only within the 30s bound. A
  single instance may still serve a cached prior value for up to 30s after its own
  write commits via another path.
- **`flag` is total.** It always returns a `bool`. A backend outage degrades flags to
  their defaults, never to an error or a hang.

## Resolution

Resolution order for **`get_raw`/`get`** (highest wins):

1. **Env var `FORGE_CFG_<KEY>`** — exact, case-sensitive name. If set (even to empty
   string), it wins.
2. **Stored value** — what `set_raw` wrote.
3. **Code default** — i.e. `None` at this surface; the caller supplies the default by
   treating `None` as "use my default".

Resolution for **`flag`**, given the resolved `FlagRule` and `ctx`:

| rule | with `targeting_key = Some(k)` | with `targeting_key = None` |
|------|-------------------------------|-----------------------------|
| `On` | `true` | `true` |
| `Off` | `false` | `false` |
| `Percent(p)` | `true` iff `stable_bucket(flag_key, k) < p` (`p` in `0..=100`) | **`default`** — cannot bucket without a key |
| `AllowList(list)` | `true` iff `k` is in `list` | **`false`** — no key can be in any list |
| *(no flag set)* | `default` | `default` |

- **`Percent(p)` is a stable hash, not a coin flip.** The bucket is
  `stable_bucket(flag_key, targeting_key)` over `0..100`, computed from the crate's
  **sha256-based** stable hash (`sha256_hex`), **never** `DefaultHasher`. Same
  `(flag_key, targeting_key)` → same bucket, **forever and across every instance and
  deploy**. A user does not flip in and out as `p` ramps; raising `p` only ever adds
  users. `Percent(0)` is always out; `Percent(100)` is always in.
- The bucket is namespaced by `flag_key`, so the same user is independently bucketed per
  flag (uncorrelated rollouts).
- **No `targeting_key`** distinguishes the two targeting rules deliberately:
  `Percent` cannot bucket, so it falls back to `default`; `AllowList` membership is
  vacuously impossible, so it resolves to `false`. `On`/`Off` ignore the context entirely.

## Limits

| thing | limit | over-limit error |
|-------|-------|------------------|
| key | ≤ 256 bytes, UTF-8, non-empty | `Invalid` |
| `get_raw` / `set_raw` value | ≤ 64 KiB (65 536 bytes) | `Limit` |
| `Percent(p)` | `p` in `0..=100` (a `u8`; values `101..=255` are invalid) | `Invalid` |
| `AllowList` size | ≤ 10 000 entries; each entry ≤ 256 bytes | `Limit` |
| cache staleness | fixed **30s** TTL (the freshness contract) | — |

Keys are matched literally; the env layer derives the variable name as
`FORGE_CFG_<KEY>` verbatim, so a key with characters illegal in an env name simply has
no env layer (store/default still resolve). The 64 KiB value cap keeps config a
key/value store, not a document store — large blobs belong in `blob`.

## Error mapping

| condition | variant | retryable |
|-----------|---------|-----------|
| `flag()` anything — missing flag, backend down, malformed rule, bad `p` | **never an error**; returns `default`, reason logged via obs | — |
| `get_raw`/`get` key unset at every layer | returns `None`, not an error | — |
| empty key, key > 256 B, `Percent(p)` with `p > 100` on `set_flag` | `Invalid` | no |
| `get::<T>` value present but fails to deserialize into `T` | `Invalid` | no |
| `set_raw` value > 64 KiB; `AllowList` over its size/entry cap | `Limit` | no |
| transient backend outage (pool timeout, dropped conn, `08xxx`/`57014`/`57P03`) | `Unavailable` | yes |
| other vendor/SDK error on a `Result` path | `Backend` (carries retryability flag) | per flag |
| bad DSN, missing migration at `Forge::init()` | `Config` | no — init only |

`flag()` is the only surface that swallows its taxonomy: by OpenFeature inheritance it
**cannot** return an error, so an `Unavailable` backend during `flag()` is logged and
becomes `default`, not a thrown error. The `Result`-returning ops (`get_raw`, `set_raw`,
`get`, `set_flag`) use the full taxonomy. `NotFound` and `Precondition` are never
produced by this surface — an unset key is `None` (read) or a no-op upsert (write), not
an error. Error messages never contain config values or flag rules — only keys' hashes,
sizes, and the resolution reason.

## Deviations from lineage

- **Boolean flags only in v1.** OpenFeature defines string/number/object/structure
  variants; Forge ships `getBooleanValue` only. The others are post-v1 and will keep the
  OpenFeature shape (`FlagRule` is `#[non_exhaustive]` to leave room).
- **Env override key is `FORGE_CFG_<KEY>`.** 12-factor leaves the env naming to the app;
  Forge fixes a single, documented prefix so the precedence chain is unambiguous and
  greppable. Other Forge subsystems use `FORGE_*` (e.g. `FORGE_POSTGRES_URL`); the
  `_CFG_` infix scopes the config keyspace.
- **30s cache staleness window is the freshness contract.** OpenFeature providers vary
  from per-call fetch to streaming push; Forge fixes a bounded-staleness cache so the
  Postgres and provider backends agree on one cheap, predictable freshness guarantee.
  No real-time push.
- **Percentage bucketing uses a fixed sha256-based stable hash.** OpenFeature leaves the
  bucketing function to the provider. Forge pins it (`sha256_hex` over
  `<flag_key>:<targeting_key>`, bucket = first bytes mod 100) so a Postgres backend and a
  provider backend bucket **identically**, and so buckets are stable across deploys —
  `DefaultHasher` is explicitly forbidden (its seed is per-process).
- **`Percent` with no `targeting_key` → `default`, `AllowList` with no key → `false`.**
  OpenFeature leaves "no targeting key" to the provider; Forge fixes the two behaviors
  distinctly (can't-bucket vs. can't-match) so the outcome is deterministic.

## Observability

Span `forge.config.<op>` (`get_raw`, `set_raw`, `flag`, `set_flag`; `get::<T>` is
recorded under `get_raw` plus a deserialize event). Fields:

| field | notes |
|-------|-------|
| `config.op` | operation name |
| `config.key_hash` | stable hash of the key — never the raw key |
| `config.source` | `get_raw`: `env` \| `store` \| `unset` (which layer resolved) |
| `config.cache` | `hit` \| `miss` (whether the 30s cache served it) |
| `config.value_bytes` | value size on `set_raw`/`get_raw` — length only, never the value |
| `flag.result` | `flag`: the resolved `bool` |
| `flag.reason` | `flag`: `targeting_match` \| `targeting_miss` \| `percent_in` \| `percent_out` \| `static` \| `default_no_key` \| `default_missing` \| `default_error` (OpenFeature reason) |
| `config.outcome` | `ok` / error variant (not emitted as error for `flag`) |

Flag values, config values, allow-list members, and `targeting_key` are **never**
emitted — only key hashes, the resolution reason, sizes, and the boolean result. A
`flag()` that fell back to `default` because of a backend error emits
`flag.reason = default_error` so silent degradation is alertable.

## Non-goals

Deliberately **not** provided (some post-v1):

- **Non-boolean flag variants** — string/number/object/structure values. Post-v1, kept
  OpenFeature-shaped.
- **Config schema management / validation** — Forge stores opaque strings; typing is the
  caller's via `get::<T>`. No schema registry.
- **Audit history / change log** — `set_raw`/`set_flag` are last-write-wins with no
  versioning or who-changed-what trail.
- **A/B experiment analytics** — flags gate behavior; measuring outcomes is out of scope.
- **Real-time push / instant invalidation** — the 30s cache is the freshness contract.
  No watch/subscribe, no sub-second propagation.
- **Multi-variant or weighted targeting beyond `Percent`/`AllowList`**, attribute-based
  rules (`EvalCtx.attributes` is reserved, unused in v1), and flag dependencies.
