# API key authentication and rotation

Forge mints API keys the way Stripe and GitHub do: an `fk_`-prefixed random secret, shown to the user exactly once, with only its SHA-256 stored at rest. You never get the plaintext back, so capture it at creation time. Verification is a constant-time hash lookup that returns the key's non-secret metadata (or `None` for an unknown or revoked key), and revocation is a single call by the key's stable id. There is no expiry in v1: keys live until revoked, so "rotation" is just create-new-then-revoke-old. Forge does not own a users table — `owner_id` is whatever opaque string your app uses to identify the principal.

## The three calls

```rust
use forge::{Forge, ForgeConfig};

#[tokio::main]
async fn main() -> forge::Result<()> {
    let forge = Forge::init(ForgeConfig::new("postgres://localhost/myapp")).await?;
    let auth = forge.auth();

    // Create: the secret is observable exactly once, here, as the return value.
    let key = auth.create_api_key("user_42", "ci-deploy").await?;
    println!("key id (safe to store/log): {}", key.id);
    println!("give this to the user once: {}", key.secret.as_str());
    // `key.secret` is an ApiKeySecret with a redacted Debug — `.as_str()` to read it.
    // There is no "reveal" path later; if it isn't captured now it's gone.

    // Verify: hand the raw `fk_...` string the client presented. Constant-time
    // lookup by hash. Live key -> Some(info); unknown or revoked -> Ok(None).
    let presented = key.secret.as_str();
    match auth.verify_api_key(presented).await? {
        Some(info) => {
            // ApiKeyInfo { id, owner_id, label } — all non-secret.
            println!("authenticated owner {} via key {}", info.owner_id, info.id);
        }
        None => println!("no live key for that secret"),
    }

    // Revoke: by the stable id, not the secret. true if a key was removed,
    // false if the id was unknown or already revoked (not an error).
    let removed = auth.revoke_api_key(&key.id).await?;
    assert!(removed);

    Ok(())
}
```

## Authenticating a request

Pull the bearer token off the header, strip the `Bearer ` prefix, and resolve it. `verify_api_key` returns the metadata you need to attach a principal to the request; `owner_id` is your app's user id.

```rust
// `token` is the raw bearer string from the Authorization header.
async fn principal(forge: &Forge, token: &str) -> Option<String> {
    let info = forge.auth().verify_api_key(token).await.ok()??;
    Some(info.owner_id)
}
```

In a real app you would typically try a session first and fall back to an API key — the chatapp example does exactly this in `principal()`, validating a session token, then a key, and mapping `owner_id` to the user. Minting is abuse-sensitive there, so it is rate-limited fail-closed before a key is issued.

## Rotation pattern

No expiry means rotation is two calls with an overlap window so in-flight clients are not cut off mid-rotation:

```rust
async fn rotate(
    forge: &Forge,
    owner_id: &str,
    label: &str,
    old_key_id: &str,
) -> forge::Result<forge::ApiKey> {
    // 1. Issue the replacement and surface its secret to the owner.
    let new_key = forge.auth().create_api_key(owner_id, label).await?;
    // 2. Hand `new_key.secret.as_str()` to the user; let them roll it out.
    // 3. Once the new key is in use, revoke the old one by its stored id.
    forge.auth().revoke_api_key(old_key_id).await?;
    Ok(new_key)
}
```

Store `ApiKey.id` (and `label`, `created_at`) in your own table when you create a key so you can list a user's keys and revoke them later — `id` is non-secret and safe to persist and log. The secret itself is never stored by you or by Forge.

## Gotchas and contract guarantees

- **The secret exists once.** `create_api_key` is the only path that ever exposes the plaintext. `ApiKeySecret`, `SessionToken`, and `PhcString` all have redacted `Debug`, and the raw key is never logged or put in spans or error messages — only the SHA-256 digest and the non-secret `key_id` are. Capture `key.secret.as_str()` immediately.
- **Verify and revoke take different arguments.** `verify_api_key` takes the raw `fk_...` *secret*; `revoke_api_key` takes the stable *id* (`ApiKey.id` / `ApiKeyInfo.id`). Don't pass the secret to revoke.
- **Absence is not an error.** `verify_api_key` returns `Ok(None)` for an unknown or revoked key, and `revoke_api_key` returns `Ok(false)` for an unknown or already-revoked id. This surface never produces `NotFound` or `Precondition`. Errors are reserved for malformed input (`Invalid`/`Limit`) and backend trouble (`Unavailable`/`Backend`).
- **No expiry in v1.** Keys live until revoked. There are no scopes either. Expiry windows and scopes are post-v1; rotation is the create-new-revoke-old pattern above.
- **Limits.** `owner_id` must be non-empty and ≤ 255 bytes UTF-8 (empty → `Invalid`, over → `Limit`); `label` may be empty but is also capped at 255 bytes. Key entropy is server-fixed at ≥ 256 bits — not a caller knob.
- **`owner_id` is yours.** Forge has no identity model; it stores and echoes the opaque string you pass. `verify_api_key` returns identity, not authorization — there is no RBAC here.

## Node / Python bindings

The binding surface is thinner than the Rust trait. In both, `verifyApiKey` / `verify_api_key` returns just the **owner id** (a `string`, or `null`/`None`), not the full `ApiKeyInfo`:

```js
// Node
const key = await forge.createApiKey("user_42", "ci-deploy"); // { id, secret }
const ownerId = await forge.verifyApiKey(key.secret);          // string | null
```

```python
# Python
key = await forge.create_api_key("user_42", "ci-deploy")   # has .id, .secret
owner_id = await forge.verify_api_key(key.secret)           # str | None
```

The created-key object exposes only `id` and `secret` in the bindings (no `label` / `created_at`). If you need the label or creation time, store them yourself at creation. Confirm whether your binding build exposes `revokeApiKey` / `revoke_api_key` before relying on it from Node/Python; Rust is the canonical surface here.
