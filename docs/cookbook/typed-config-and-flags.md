# Typed config and feature flags

Forge splits "settings you read" from "flags you gate behavior on", both behind one store reachable at `forge.config()` (which hands you a `&dyn ConfigStore`). For settings, bind a key to a Rust type once with `ConfigKey<T>` and read it through `ConfigTyped` (`get_typed` / `get_or_default` / `set_typed`); the resolved value follows a fixed precedence chain — `FORGE_CFG_<KEY>` env var beats the stored value beats your code default. For boolean rollouts, use `flag()` / `set_flag()` with a `FlagRule` (`On`, `Off`, `Percent(p)`, `AllowList(..)`) and an `EvalCtx`. The load-bearing guarantee: `flag()` returns `bool`, never `Result` — any failure resolves to the `default` you pass.

```rust
use forge::{ConfigKey, ConfigTyped, ConfigStore, EvalCtx, Forge, FlagRule};

// Declare each setting once: key string + value type + default, together.
// These are cheap to construct; keep them as module constants/statics or build
// them inline — `ConfigKey::new` is not `const` (the default value is owned).
fn max_upload_bytes() -> ConfigKey<u64> {
    ConfigKey::new("max_upload_bytes", 5 * 1024 * 1024) // 5 MiB floor
}

async fn demo(forge: &Forge) -> forge::Result<()> {
    let cfg = forge.config(); // &dyn ConfigStore

    // --- Typed config ---------------------------------------------------------

    // Read with the code default as the floor. Resolution: FORGE_CFG_MAX_UPLOAD_BYTES
    // env var, else the stored value, else the ConfigKey default (5 MiB here).
    let limit: u64 = cfg.get_or_default(&max_upload_bytes()).await?;

    // Or distinguish "unset everywhere" (None) from a set value.
    if let Some(stored) = cfg.get_typed(&max_upload_bytes()).await? {
        println!("operator set the limit to {stored}");
    }

    // Write the stored layer (last-write-wins, JSON-encoded). An active
    // FORGE_CFG_MAX_UPLOAD_BYTES env var still shadows this — set_typed does not
    // touch the environment.
    cfg.set_typed(&max_upload_bytes(), &(10 * 1024 * 1024u64)).await?;

    // --- Feature flags --------------------------------------------------------

    // Roll out to 25% of users. Bucketing is a stable sha256 over (flag_key,
    // targeting_key): the same user stays in or out as you ramp p; raising p only
    // ever adds users.
    cfg.set_flag("reactions_v2", FlagRule::Percent(25)).await?;

    // Evaluate for one user. `flag` is total: on a missing flag, a backend outage,
    // or a malformed rule it returns the default you pass (here `false`) and logs
    // the reason — it never errors and never panics.
    let user_id = "user-42";
    let on = cfg
        .flag("reactions_v2", false, &EvalCtx::user(user_id))
        .await;
    println!("reactions_v2 for {user_id}: {on}");

    // Other rule shapes:
    cfg.set_flag("kill_switch", FlagRule::Off).await?;          // always false
    cfg.set_flag("beta", FlagRule::AllowList(vec![             // listed keys only
        "user-1".into(),
        "user-7".into(),
    ])).await?;

    // No targeting key: On/Off ignore it; Percent falls back to `default`;
    // AllowList resolves to `false` (no key can be in any list).
    let anon = cfg.flag("beta", false, &EvalCtx::new()).await; // -> false
    let _ = (limit, anon);
    Ok(())
}
```

## Notes and gotchas

- **Precedence (config reads).** `get_raw`/`get_typed`/`get_or_default` resolve `FORGE_CFG_<KEY>` env (exact, case-sensitive name; set even to empty string wins) → stored value → `None`/default. The env name is derived from the key verbatim, so a key with characters illegal in an env var name simply has no env layer (store and default still resolve). The prefix is `FORGE_CFG_` specifically — distinct from other subsystems' `FORGE_*` (e.g. `FORGE_POSTGRES_URL`).
- **30s staleness, not strong consistency.** Reads are served from an in-process cache with a fixed 30s TTL (`config_store::CACHE_TTL_SECS`). A committed `set_raw`/`set_typed`/`set_flag` is guaranteed visible at every reader within 30s; until then the cache may serve the prior value. Read-your-writes is not guaranteed across instances, only within that bound. There is no push/invalidation.
- **`Percent` semantics.** Bucket is `sha256_hex(<flag_key>:<targeting_key>) mod 100`, stable forever and across deploys/instances, and namespaced per flag (rollouts don't correlate). `Percent(0)` is always out, `Percent(100)` always in. `p` is a `u8` in `0..=100`; `set_flag` with `p > 100` is `ForgeError::Invalid`.
- **Flags fail to default.** Design rollouts so "everyone gets the default" is a safe state. A `flag()` that degraded to its default because the backend was down still logs `flag.reason = default_error`, so silent degradation is alertable.
- **`get_typed` vs `get`.** `ConfigTyped::get_typed` (from `forge::ConfigTyped`) takes a `ConfigKey<T>`; the lower-level `ConfigExt::get::<T>(key_str)` (from `forge::ConfigExt`) takes a raw key string. Both deserialize the resolved value as JSON; a present value that fails to parse is `ForgeError::Invalid`, not `None`.
- **Limits.** Key ≤ 256 bytes UTF-8 non-empty; value ≤ 64 KiB (it's a key/value store — large blobs belong in `blob`); `AllowList` ≤ 10,000 entries, each ≤ 256 bytes. Over-limit keys are `Invalid`; over-limit values/lists are `Limit`.
- **v1 is boolean-only.** `FlagRule` is `#[non_exhaustive]`; non-boolean OpenFeature variants and attribute-based targeting are post-v1 (v1 rules read only `targeting_key`).

**Node / Python.** The same `FORGE_CFG_*` precedence, the 30s cache bound, and the flag rules/evaluation are identical across bindings (the typed config layer mirrors these Rust definitions). The backend-selection knobs and config keyspace are driven by the same `FORGE_*` / `FORGE_CFG_*` env vars in every language, so there's no per-language config API to learn.
