# Frontend Playbook

This reference outlines the core principles and architectural patterns for building frontends with Forge. For framework-specific implementation details, refer to the SvelteKit (`svelte.md`) and Dioxus (`dioxus.md`) guides.

## Core Development Rules

- **The backend is the source of truth**: Always define your data contracts on the backend first. This ensures that your frontend types are derived from a single, consistent source.
- **Strict development workflow**: **MANDATE:** Always run `forge generate` after backend changes. Never edit generated files. See [Pitfalls](./pitfalls.md#1-generated-code).

## Reactivity Model

Forge uses a server-driven reactivity model to keep the frontend in sync with the database.

- **Query-based subscriptions**: The backend query is the fundamental unit of reactivity. When you subscribe to a query, the server monitors relevant database changes.
- **Server-driven updates**: When the database changes, the server re-executes the query, hashes the result, and pushes updates to the client via Server-Sent Events (SSE) only if the data has actually changed.
- **No manual refetching**: You do not need to implement manual refetching, WebSockets, or complex cache invalidation logic. The framework handles data synchronization automatically.

## Subscription State Shape

All subscription stores and hooks return a consistent state object to help you manage the UI lifecycle.

```typescript
{
  loading: boolean,      // True until the first data packet is received.
  data: T | null,        // The current result of the query.
  error: Error | null,   // Contains the last error encountered during the subscription.
  stale: boolean         // True if the client is currently disconnected and attempting to reconnect.
}
```

### Specialized Statuses
- **Jobs and Workflows**: These states include additional fields such as `jobId`, `status`, `progress`, and `output`.
- **Terminal Errors**: Statuses like `blocked_missing_version`, `blocked_signature_mismatch`, or `blocked_missing_handler` indicate an operational error (e.g., a version mismatch between frontend and backend). These should be displayed as critical system errors.

## Authentication and Session Management

- **Configuration**: Set your `access_token_ttl` and `refresh_token_ttl` in `forge.toml` to control session duration.
- **Token Issuance**: Use `ctx.issue_token_pair()` on the backend to generate JWTs. See [Patterns Reference](./patterns.md#2-authentication-and-authorization).
- **Session Continuity**: The client must reconnect its SSE stream whenever the authentication principal changes (e.g., after a login or logout). Mismatching a session with a new user will cause errors.

- **Persistence**: Store tokens and user information in `localStorage` and implement a periodic refresh loop to maintain the session.

## Error Handling Logic

- **Structured Errors**: Forge returns wire errors in a `{ code, message, retry_after_secs?, details? }` format. Svelte exposes this as `ForgeClientError.retryAfterSecs`; Dioxus exposes the Rust field as `retry_after_secs`.
- **Control Flow**: Use the error `code` (e.g., `NOT_FOUND`, `RATE_LIMITED`) for programmatic logic and the `message` for user-facing display.
- **Boolean helpers**: Svelte errors expose `.isRateLimited()`, `.isUnauthorized()`, and `.isValidation()`. Dioxus errors expose `.is_rate_limited()`, `.is_unauthorized()`, and `.is_validation()`.
- **Automatic Cooldowns**: Use `retryAfterSecs` in Svelte or `retry_after_secs` in Dioxus to implement UI-level cooldown timers for rate-limited operations.
- **Managed Retries**: The client library automatically handles SSE reconnection with exponential backoff. Do not implement custom retry loops for subscriptions.

## File Uploads

- **Multipart Support**: Mutations using `Upload` types automatically switch to `multipart/form-data`.
- **Supported Types**: You can use `Upload`, `Vec<Upload>`, or `Option<Upload>` in your mutation parameters.
- **Default Constraints**: The maximum body size is 20MB by default, but individual JSON fields are limited to 1MB. Field names must be under 255 characters.
- **Large Files**: For files significantly larger than 20MB, use the backend to generate presigned S3/GCS URLs and upload directly from the browser.

## Signals (Analytics and Diagnostics)

Signals are automatically initialized by `ForgeProvider`. No manual setup needed for basic analytics.

### What's auto-captured

Page views (SPA navigation), frontend errors (window.onerror, unhandledrejection), Web Vitals (LCP, CLS, INP, FCP, TTFB, navigation timing, long tasks), online/offline transitions, and correlation IDs linking frontend events to backend RPC calls.

### Manual instrumentation (Svelte)

Access the signals instance via `getForgeSignals()` from any component inside `ForgeProvider`:

```svelte
<script>
  import { getForgeSignals } from '@forge-rs/svelte';

  const signals = getForgeSignals();

  // Track custom events
  signals.track('button_click', { target: 'upgrade-plan', variant: 'A' });

  // Identify authenticated users (call on login/signup)
  signals.identify(user.id, { name: user.name, plan: 'pro' });

  // Add breadcrumbs for error context (attached to the next captured error)
  signals.breadcrumb('Opened settings modal', { tab: 'billing' });

  // Report errors manually (auto-capture handles most cases)
  signals.captureError(new Error('Payment failed'), { orderId: '123' });

  // Manual page view (only needed if autoPageViews is disabled)
  await signals.page({ section: 'onboarding' });

  // Manual Web Vital (only needed if autoWebVitals is disabled)
  signals.vital('custom-metric', 42, { rating: 'good' });
</script>
```

### Manual instrumentation (Dioxus)

```rust
use forge_dioxus::use_signals;

let signals = use_signals();

// Track custom events
signals.track_with_properties("button_click", json!({"target": "upgrade-plan"}));

// Identify authenticated users
signals.identify("user-id", json!({"name": "Alice", "plan": "pro"}));

// Breadcrumbs for error context
signals.breadcrumb("Opened settings", Some(json!({"tab": "billing"})));

// Report errors
signals.capture_error("Payment failed", Some(json!({"order_id": "123"})));

// Manual page view
signals.page("/settings");
```

### Correlation IDs

Every RPC call automatically includes an `x-correlation-id` header linking the frontend event to the backend execution trace. Use `signals.nextCorrelationId()` (Svelte) or `signals.next_correlation_id()` (Dioxus) if you need to generate one manually for non-RPC requests.

### Privacy

Daily-rotating hashed visitor IDs, no cookies. Set `anonymize_ip = true` in `[signals]` config to drop raw IPs. The SDK honors `DNT: 1` and `Sec-GPC: 1` (`respectDnt: true` by default) and the server short-circuits signal ingestion when those headers arrive. Crash reports still land so production errors from DNT users don't disappear.

### GeoIP

GeoIP has two backends. The default build (`geoip` feature, in `full`) is a runtime MaxMind MMDB reader: set `geoip_db_path` in `[signals]` to a GeoLite2-City MMDB to populate the `country` and `city` columns of `forge_signals_events`. For zero-config country resolution with no MMDB file, build with the `geoip-embedded` feature, which bakes in a DB-IP Country Lite database (the only geoip option needing a build-time download).

### Client config

Pass `SignalsConfig` to the provider to customize behavior:

| Option | Default | Description |
|---|---|---|
| `enabled` | `true` | Master switch for all signal collection |
| `autoPageViews` | `true` | Track SPA navigation automatically |
| `autoCaptureErrors` | `true` | Capture unhandled errors and rejections |
| `autoWebVitals` | `true` | Collect Core Web Vitals and performance timing |
| `autoNetworkEvents` | `true` | Track online/offline transitions |
| `respectDnt` | `true` | Honor DNT and GPC browser signals |
| `persistQueue` | `true` | Persist event queue to localStorage (Svelte only) |
| `flushInterval` | `5000` | Milliseconds between batch flushes |
| `maxBatchSize` | `20` | Max events per batch |

### Event batching

Events are queued in memory, persisted to `localStorage` (Svelte) so they survive page reloads, and flushed on an interval, on `visibilitychange`/`pagehide`, or when the network returns online. Dioxus WASM uses `sendBeacon` for flush-on-unload.

## Forbidden Practices

- **Manual refetch loops**: These cause unnecessary server load and UI flickering; use live subscriptions instead.
- **Client-side only auth**: Never rely on frontend permission checks; always validate authorization on the backend.
- **Skipping generation**: Failing to run `forge generate` after backend changes will lead to runtime type mismatches.
