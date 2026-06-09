# blob — lineage: AWS S3 API

Object storage for files: uploads, exports, attachments, generated artifacts.

## Lineage

Verbs and semantics mirror the **AWS S3** object API: `PutObject`, `GetObject`,
`HeadObject`, `DeleteObject`, `ListObjectsV2`, and presigned URLs. Method names are
Rust-idiomatic (`put`/`get`/`head`/`delete`/`list`/`presign_*`); behavior matches S3.
The byte-stream value type and key-as-path convention align with **wasi-blobstore**.
The default backend is Postgres (`forge_blobs`, `BYTEA`); the contract is the lowest
common denominator that Postgres **and** S3 can both honor, so an S3-backed
implementation stays a drop-in later with no app-code change.

Purpose: storing and serving discrete binary objects keyed by path. Not a CDN, not a
filesystem, not a relational store, not a message bus.

## Trait (Rust sketch — directional; this doc wins on conflict)

```rust
#[async_trait]
pub trait Blob: Send + Sync {
    /// S3 PutObject. Buffered, not streamed (<= 50 MiB in v1). Last-write-wins
    /// on an existing key. Returns Ok(()) — the new ETag is read via head.
    async fn put(&self, key: &str, data: Bytes, opts: PutOpts) -> Result<()>;

    /// S3 GetObject. None if the key is absent.
    async fn get(&self, key: &str) -> Result<Option<Bytes>>;

    /// S3 HeadObject. Metadata only, no body. None if absent.
    async fn head(&self, key: &str) -> Result<Option<BlobInfo>>;

    /// S3 DeleteObject. true if an object was removed, false if it was already absent.
    async fn delete(&self, key: &str) -> Result<bool>;

    /// S3 ListObjectsV2. Lexicographic key order, prefix-filtered, cursor-paginated.
    async fn list(&self, prefix: &str, cursor: Option<Cursor>, limit: u32)
        -> Result<ListPage>;

    /// HMAC-SHA256-signed, single-key-scoped, time-bound, size-capped upload URL.
    async fn presign_upload(&self, key: &str, expires: Duration, max_bytes: u64)
        -> Result<Url>;

    /// HMAC-SHA256-signed, single-key-scoped, time-bound download URL.
    async fn presign_download(&self, key: &str, expires: Duration) -> Result<Url>;
}

#[non_exhaustive]
pub struct PutOpts {
    /// S3 Content-Type. Stored verbatim, echoed by head/download. Default
    /// "application/octet-stream".
    pub content_type: Option<String>,
    /// S3 x-amz-meta-*. Opaque user metadata, round-tripped on head. Default empty.
    pub metadata: BTreeMap<String, String>,
}

#[non_exhaustive]
pub struct BlobInfo {
    pub key: String,
    pub size: u64,
    pub content_type: String,
    /// Content hash, hex (NOT guaranteed S3-MD5-shaped). Changes iff bytes change.
    pub etag: String,
    pub last_modified: SystemTime, // TIMESTAMPTZ, seconds precision
    pub metadata: BTreeMap<String, String>,
}

#[non_exhaustive]
pub struct ListPage { pub items: Vec<BlobInfo>, pub next: Option<Cursor> }

pub struct Cursor(/* opaque, backend-owned */);
```

`Bytes` is the object body (opaque, never interpreted). `Cursor` is opaque: callers
pass back exactly what `list` returned, never construct it. `Url` is a fully-formed,
ready-to-use URL string.

## Semantics

| op | behavior |
|----|----------|
| `put` | Writes the object at `key`, **last-write-wins**: an existing object at the same key is fully replaced (body, `content_type`, `metadata`, `etag`, `last_modified`). No partial merge of metadata. Computes a fresh `etag` from the body. `Ok(())` on success — to read the new `etag`, call `head`. |
| `get` | Returns `Some(body)` if the key exists, else `None`. Returns the exact bytes last written. |
| `head` | Returns `Some(BlobInfo)` (metadata, no body) if the key exists, else `None`. `size` is the byte length; `etag` is the content hash; `last_modified` is the last `put`'s commit time at seconds precision. |
| `delete` | Removes the object. Returns `true` if an object was removed, `false` if the key was already absent. Idempotent: a second `delete` returns `false`, not an error. |
| `list` | Returns up to `limit` `BlobInfo`s whose key starts with `prefix`, in **lexicographic key order**, plus a `Cursor` for the next page (`None` when iteration is complete). Cursor-based only — no offset. An empty `prefix` lists the whole namespace. |
| `presign_upload` | Returns a URL that authorizes a single `PUT` of **this one key**, valid for `expires`, rejecting bodies over `max_bytes`. The upload, when performed, behaves exactly like `put` (last-write-wins, fresh `etag`). |
| `presign_download` | Returns a URL that authorizes a single `GET` of **this one key**, valid for `expires`. Resolves to the object's current bytes at fetch time (no snapshot). A download of a since-deleted key yields a `404` at the router. |

Keys are `/`-delimited path strings (`exports/2026/report.csv`). The `/` is an
ordinary key byte, not a directory separator — there are no directories, only a flat
keyspace with lexicographic ordering, and `prefix` matching gives the
folder-like listing (S3 semantics). `content_type` and `metadata` are stored verbatim
and round-tripped; Forge does not validate, sniff, or rewrite them.

## Delivery / consistency guarantees

- **Last-write-wins.** Concurrent `put`s to one key resolve to whichever commits last;
  no merge, no versioning. The surviving object is internally consistent (its `etag`,
  `size`, and `content_type` all describe the same surviving body — never torn).
- **Read-after-write on a single key.** A `get`/`head` issued after a `put` returns
  succeeds observes that write (read-committed). A reader racing a concurrent `put`
  sees either the whole old object or the whole new one, never a mix.
- **Per-key atomicity only.** Each `put`/`delete` is atomic for its one key. There are
  **no multi-key transactions** and no cross-key snapshot. `list` is a weakly
  consistent iteration: objects present for the whole listing appear; objects added or
  removed mid-listing may or may not appear.

## Ordering

`list` returns objects in **lexicographic (bytewise) order of the full key**, S3
`ListObjectsV2` semantics. Pagination is stable forward: a `Cursor` resumes after the
last key of the previous page. Keys inserted before the cursor position after a page
was returned will not retroactively appear; keys inserted ahead of the cursor may
appear on a later page. No reverse order, no sort by size/time. There is no ordering
relationship across distinct keys for `put`/`delete` beyond per-key linearizability.

## ETag / resolution

- **`etag` is a content hash** (hex of a hash over the body), recomputed on every
  `put`. Equal bytes => equal `etag`; any byte change => a different `etag`. It is
  **not** guaranteed to be S3-MD5-shaped (see Deviations) — treat it as an opaque
  change-detection token, not a checksum of a known algorithm.
- **Presigned URLs resolve through a mounted router.** On the Postgres backend the
  signed URL points at Forge's optional axum router (`forge.blob_router()`,
  feature-gated) that the app mounts. The router verifies the HMAC signature, the
  expiry, the key scope, and (on upload) the `max_bytes` cap, then performs the
  equivalent `get`/`put` against `forge_blobs`. Signing and verification are identical
  to the S3 path, so swapping to S3 changes nothing in app code — only the URL host.
- **Signature scope.** Each URL is bound to one `key`, one method (`GET` xor `PUT`),
  one expiry, and (upload) one `max_bytes`. A URL cannot be replayed against a
  different key, method, or a larger body. Expiry is at seconds precision (`TIMESTAMPTZ`).
- **No snapshot semantics.** A presigned download serves the object's bytes **at fetch
  time**, not at sign time. If the object is overwritten or deleted between signing and
  fetching, the fetch reflects the current state (current bytes, or `404`).

## Limits

| thing | limit | over-limit error |
|-------|-------|------------------|
| object body (`put`) | <= 50 MiB (52 428 800 bytes) | `Limit` |
| key | <= 1024 bytes, UTF-8 | `Limit` |
| `content_type` | <= 256 bytes | `Limit` |
| metadata: total of all keys + values | <= 2 KiB (2048 bytes) | `Limit` |
| `list` limit | advisory, clamped to <= 1000; backend may return fewer | clamped, not an error |
| presign `expires` | `> 0`, `<=` 7 days | `Invalid` if zero/negative; `Limit` if over the ceiling |
| presign upload `max_bytes` | <= 50 MiB (the `put` ceiling) | `Limit` |

The 50 MiB body cap matches the v1 `put` path: bodies are buffered fully in memory and
stored as `BYTEA`, so streaming and multipart (which would lift this toward S3's
5 GiB/`PutObject`) are post-v1. The `list` limit ceiling of 1000 mirrors
`ListObjectsV2`'s `MaxKeys`; a larger request is clamped, not rejected. The 7-day
presign ceiling matches S3 SigV4's documented maximum. Keys are UTF-8; the 1024-byte
cap is on the encoded byte length, not character count.

## Error mapping

| condition | variant | retryable |
|-----------|---------|-----------|
| `get`/`head` on a missing key | returns `Ok(None)`, not an error | — |
| `delete` on a missing key | returns `Ok(false)`, not an error | — |
| object body > 50 MiB; key/content_type/metadata over cap; `max_bytes` over ceiling | `Limit` | no |
| presign `expires` over the 7-day ceiling | `Limit` | no |
| empty key; presign `expires` zero/negative | `Invalid` | no — caller bug |
| transient backend outage (pool timeout, dropped conn, `08xxx`/`57014`) | `Unavailable` | yes |
| router: presigned request with a bad/tampered or expired signature | `Precondition` | no — re-sign |
| router: presigned upload body exceeds the signed `max_bytes` | `Precondition` | no — fence miss |
| router: malformed presigned request (missing/garbled signing params) | `Invalid` | no — caller bug |
| misconfiguration (bad DSN, missing migration, router mounted without signing secret) at `Forge::init()` | `Config` | no — init only |
| other vendor/SDK error | `Backend` (carries retryability flag) | per flag |

`NotFound` is **never** produced by this surface. Absence on a read path is
`Ok(None)`/`Ok(false)`, matching S3's "missing object => no object" on these methods
(S3 returns a `404`; the trait normalizes that to `None`/`false`). A presigned request
that fails *verification* (expired, tampered, wrong key/method, oversized upload)
yields `Precondition` (the signed precondition no longer holds), while a *malformed*
request (a caller that built the URL wrong) yields `Invalid`. `Config` is init-time
only: a router mounted without a signing secret fails at `Forge::init()`, never lazily
on first presign. Error messages never contain object bytes, key contents, metadata
values, or signing secrets.

## Deviations from lineage

- **Presign is HMAC-SHA256 over an app secret, not AWS SigV4.** S3 presigned URLs use
  SigV4 over AWS credentials. Forge signs with HMAC-SHA256 over a Forge-configured app
  secret, scoped to one key/method/expiry (and `max_bytes` on upload). Same guarantees
  (single-key, time-bound, size-capped, unforgeable), simpler scheme, no AWS
  credential machinery.
- **Presigned URLs require the mounted router on the Postgres backend.** S3 URLs
  resolve at AWS. Forge's Postgres URLs resolve only if the app has mounted
  `forge.blob_router()`; without it, signed URLs do not resolve. (On an S3 backend the
  URLs point at S3 directly and no router is needed — app code is unchanged.)
- **50 MiB object cap (S3 allows 5 GiB per `PutObject`).** v1 buffers the whole body in
  memory and stores `BYTEA`. Larger objects need streaming + multipart, which are
  post-v1. Over => `Limit`.
- **`etag` is a content hash, not guaranteed S3-MD5-shaped.** S3's ETag for a
  single-part object is the body's MD5 hex; Forge's `etag` is *a* content hash and may
  differ in algorithm/shape. Both satisfy "changes iff the bytes change"; do not parse
  Forge's `etag` as an MD5.
- **`put` returns `Ok(())`, not the new ETag.** S3 `PutObject` returns the ETag in the
  response. Forge returns unit and exposes the new `etag` via `head`, keeping the write
  path's return shape minimal; one extra round-trip when the ETag is needed.
- **`list` is prefix + lexicographic only.** S3 `ListObjectsV2` also supports a
  `delimiter` (for `CommonPrefixes` folder rollups), `start-after`, and owner/storage
  fields. Forge exposes a flat prefix listing with cursor pagination; no delimiter
  rollup, no per-object owner/storage-class fields.

## Observability

One span per operation, emitted automatically. Span name `forge.blob.<op>`
(`put` / `get` / `head` / `delete` / `list` / `presign_upload` / `presign_download`),
plus `forge.blob.router.<get|put>` for a verified router request.

Fields (never object bytes, never key contents, never metadata values, never signing
secrets or signatures):

| field | notes |
|-------|-------|
| `blob.op` | operation name |
| `blob.key_hash` | stable hash of the key — never the raw key |
| `blob.namespace` | configured namespace prefix |
| `blob.hit` | `get`/`head`/`delete`: whether an object was found |
| `blob.size_bytes` | object size on `put` / `get` / `head` |
| `blob.etag` | content hash (the object's public change token, not a secret) |
| `blob.content_type` | resolved content type |
| `blob.meta_count` | number of user metadata entries (not keys/values) |
| `blob.list_returned` | `list`: count of items in this page |
| `blob.presign_expires_secs` | resolved presign lifetime in seconds |
| `blob.presign_max_bytes` | upload size cap, on `presign_upload` |
| `blob.outcome` | `ok` / error variant |

Keys, bytes, metadata values, signatures, and the signing secret are **never** emitted
— only hashes, sizes, counts, and the (already public) `etag`/`content_type`.

## Non-goals

- **No streaming I/O.** Whole-body `put`/`get` only in v1; ranged/streamed reads and
  writes are post-v1.
- **No multipart upload.** Single-shot `put` only; the 50 MiB cap follows from it.
- **No versioning.** Last-write-wins; overwrites are not retained. No version ids, no
  history.
- **No lifecycle policies.** No automatic expiry, tiering, or transition rules.
  Deletion is explicit via `delete`.
- **No public-bucket / ACL semantics.** Access is via presigned URLs or in-process
  calls only; there is no anonymous or policy-based public access model.
- **No server-side copy / move / rename.** No `CopyObject`. To relocate an object,
  `get` then `put` then `delete` from the caller.
- **No multi-key transactions** and no cross-key atomicity or snapshots.
- **Not a CDN.** No edge caching, no cache-control orchestration; the router serves
  bytes directly.
