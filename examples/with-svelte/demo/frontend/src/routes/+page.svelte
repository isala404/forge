<script lang="ts">
  import {
    createUser,
    updateUser,
    deleteUser,
    login,
    register,
    refreshToken,
    getDemoStats,
    trackExportUsers,
    trackAccountVerification,
    confirmVerification,
    getUsers$,
    getIssLocation$,
    getTrades$,
    getWebhookEvents$,
    type User,
    type AuthResponse,
    type TokenPair,
    type DemoStats,
  } from "$lib/forge";

  import { PUBLIC_API_URL } from "$env/static/public";
  import { auth } from "$lib/forge/auth.svelte";
  import { getForgeSignals } from "@forge-rs/svelte";
  const signals = getForgeSignals();
  const apiUrl = PUBLIC_API_URL;

  const users = getUsers$();
  const issLocation = getIssLocation$();
  const trades = getTrades$();
  const webhookEvents = getWebhookEvents$();

  // User CRUD state
  let name = $state("");
  let email = $state("");
  let isSubmitting = $state(false);
  let selectedUser = $state<User | null>(null);
  let editingUserId = $state<string | null>(null);
  let editName = $state("");
  let editEmail = $state("");
  let isEditing = $state(false);
  let deletePopoverUserId = $state<string | null>(null);
  let crudError = $state<string | null>(null);

  // Export / Workflow
  let exportJobStore = $state<ReturnType<typeof trackExportUsers> | null>(null);
  let workflowStore = $state<ReturnType<typeof trackAccountVerification> | null>(null);
  let confirmSent = $state(false);

  // Webhook
  let idempotencyKey = $state(generateKey());
  let keyUsed = $state(false);
  let webhookError = $state<string | null>(null);

  // Auth form state (only form inputs and UI state are local)
  let authMode = $state<"login" | "register">("login");
  let authEmail = $state("demo@example.com");
  let authPassword = $state("password123");
  let authName = $state("");
  let authLoading = $state(false);
  let authError = $state<string | null>(null);
  let refreshCount = $state(0);

  // Derived from the auth store (persisted across refreshes)
  let tokenClaims = $derived(auth.token ? parseJwtClaims(auth.token) : null);

  // Cache demo state
  let cacheData = $state<DemoStats | null>(null);
  let responseMs = $state<number | null>(null);
  let fetchCount = $state(0);
  let cacheLoading = $state(false);

  function generateKey(): string {
    return `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  }

  function formatTimestamp(ts: number): string {
    return new Date(ts * 1000).toLocaleTimeString();
  }

  function parseJwtClaims(token: string): [string, string][] {
    try {
      const base64Url = token.split(".")[1];
      const base64 = base64Url.replace(/-/g, "+").replace(/_/g, "/");
      const json = decodeURIComponent(
        atob(base64)
          .split("")
          .map((c) => "%" + ("00" + c.charCodeAt(0).toString(16)).slice(-2))
          .join("")
      );
      const obj = JSON.parse(json);
      const displayKeys = ["sub", "roles", "iat", "exp"];
      const result: [string, string][] = [];
      for (const key of displayKeys) {
        if (key in obj) {
          const val = obj[key];
          const formatted =
            key === "iat" || key === "exp"
              ? typeof val === "number"
                ? formatTimestamp(val)
                : String(val)
              : JSON.stringify(val);
          result.push([key, formatted]);
        }
      }
      return result;
    } catch {
      return [];
    }
  }

  async function handleAuth(e: Event) {
    e.preventDefault();
    signals.breadcrumb(`Auth ${authMode} attempt`);
    signals.track("auth_attempt", { mode: authMode });
    authLoading = true;
    authError = null;
    try {
      let res: AuthResponse;
      if (authMode === "register") {
        res = await register({ email: authEmail, name: authName, password: authPassword });
      } else {
        res = await login({ email: authEmail, password: authPassword });
      }
      auth.setAuth(res.access_token, res.refresh_token, res.user);
      signals.track("auth_success", { mode: authMode, user_id: res.user.id });
      refreshCount = 0;
    } catch (err: unknown) {
      authError = err instanceof Error ? err.message : String(err);
      signals.track("auth_error", { mode: authMode, error: authError });
    } finally {
      authLoading = false;
    }
  }

  async function handleRefresh() {
    if (!auth.refreshToken) return;
    authError = null;
    try {
      const pair: TokenPair = await refreshToken({ refresh_token: auth.refreshToken });
      auth.updateTokens(pair.access_token, pair.refresh_token);
      refreshCount++;
      signals.track("token_refresh", { count: refreshCount });
    } catch (err: unknown) {
      authError = err instanceof Error ? err.message : String(err);
    }
  }

  function handleLogout() {
    signals.track("logout");
    auth.clearAuth();
    refreshCount = 0;
    authError = null;
  }

  async function handleFetchStats() {
    cacheLoading = true;
    const start = performance.now();
    try {
      const stats = await getDemoStats();
      const elapsed = performance.now() - start;
      cacheData = stats;
      responseMs = elapsed;
      fetchCount++;
      signals.track("cache_fetch", { response_ms: elapsed, cache_hit: elapsed < 100, fetch_number: fetchCount });
    } catch {
      // ignore
    }
    cacheLoading = false;
  }

  async function triggerWebhook() {
    signals.breadcrumb("Sending webhook");
    webhookError = null;
    const secret = "demo-secret";
    const payload = JSON.stringify({ action: "test", ts: Date.now() });

    const encoder = new TextEncoder();
    const key = await crypto.subtle.importKey(
      "raw",
      encoder.encode(secret),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"]
    );
    const signature = await crypto.subtle.sign("HMAC", key, encoder.encode(payload));
    const signatureHex = Array.from(new Uint8Array(signature))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");

    const res = await fetch(`${apiUrl}/_api/webhooks/demo`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Webhook-Signature": signatureHex,
        "X-Webhook-Timestamp": Math.floor(Date.now() / 1000).toString(),
        "X-Idempotency-Key": idempotencyKey,
      },
      body: payload,
    });

    if (res.ok) {
      keyUsed = true;
      signals.track("webhook_sent", { idempotency_key: idempotencyKey });
    } else {
      webhookError = `Error: ${res.status}`;
      signals.track("webhook_error", { status: res.status });
    }
  }

  function newKey() {
    idempotencyKey = generateKey();
    keyUsed = false;
    webhookError = null;
    signals.track("webhook_key_generated");
  }

  async function handleCreateUser(e: Event) {
    e.preventDefault();
    const n = name.trim();
    const em = email.trim();
    if (!n || !em || isSubmitting) return;
    signals.breadcrumb("Creating user");
    crudError = null;
    isSubmitting = true;
    try {
      await createUser({ email: em, name: n, role: null });
      signals.track("user_created", { name: n });
      name = "";
      email = "";
    } catch (err: unknown) {
      crudError = err instanceof Error ? err.message : String(err);
      signals.track("user_create_error");
    }
    isSubmitting = false;
  }

  function startEditUser(user: User) {
    signals.track("user_edit_start", { user_id: user.id });
    selectedUser = user;
    editingUserId = user.id;
    editName = user.name;
    editEmail = user.email;
    deletePopoverUserId = null;
  }

  async function handleUpdateUser() {
    if (!editingUserId || isEditing) return;
    signals.breadcrumb("Updating user");
    isEditing = true;
    crudError = null;
    try {
      await updateUser({ id: editingUserId, name: editName, email: editEmail, role: null });
      signals.track("user_updated", { user_id: editingUserId });
      editingUserId = null;
    } catch (err: unknown) {
      crudError = err instanceof Error ? err.message : String(err);
      signals.track("user_update_error");
    }
    isEditing = false;
  }

  function cancelEdit() {
    editingUserId = null;
  }

  async function confirmDeleteUser(id: string) {
    signals.breadcrumb("Deleting user");
    deletePopoverUserId = null;
    crudError = null;
    try {
      await deleteUser({ id });
      signals.track("user_deleted", { user_id: id });
      if (selectedUser?.id === id) selectedUser = null;
    } catch (err: unknown) {
      crudError = err instanceof Error ? err.message : String(err);
      signals.track("user_delete_error");
    }
  }

  function startExportJob() {
    signals.track("export_started", { format: "csv" });
    exportJobStore = trackExportUsers({ format: "csv" });
  }

  function startVerificationWorkflow() {
    const account_id = selectedUser?.id || "demo-user";
    const email = selectedUser?.email || "demo@example.com";
    signals.track("workflow_started", { type: "verification", account_id, email });
    confirmSent = false;
    workflowStore = trackAccountVerification({ account_id, email });
  }

  async function handleConfirmVerification() {
    if (!workflowStore || confirmSent) return;
    const state = $workflowStore;
    if (!state?.workflowId) return;
    confirmSent = true;
    signals.track("workflow_confirmed", { workflow_id: state.workflowId });
    try {
      await confirmVerification({ workflow_id: state.workflowId });
    } catch (err: unknown) {
      confirmSent = false;
      console.error("Failed to confirm verification:", err);
    }
  }

  function formatCoord(value: number, isLat: boolean): string {
    const dir = isLat ? (value >= 0 ? "N" : "S") : value >= 0 ? "E" : "W";
    return `${Math.abs(value).toFixed(4)}\u00a0${dir}`;
  }

  function formatTime(ts: string): string {
    return ts ? new Date(ts).toLocaleTimeString() : "-";
  }

  function formatPrice(price: number): string {
    return price.toLocaleString(undefined, { minimumFractionDigits: 5, maximumFractionDigits: 5 });
  }

  function stepIcon(status: string): string {
    if (status === "completed") return "[=]";
    if (status === "running") return "[>]";
    if (status === "failed") return "[x]";
    return "[ ]";
  }
</script>

<main class="shell">
  <div class="columns">
    <!-- Left column -->
    <div class="col">
      <section class="card dark">
        <h2>ISS Location <span class="badge">cron</span></h2>
        <div class="stats">
          <div>
            <span class="label">Lat</span>
            <span class="value" class:placeholder={!issLocation.data}>{issLocation.data ? formatCoord(issLocation.data.latitude, true) : "---.----"}</span>
          </div>
          <div>
            <span class="label">Lon</span>
            <span class="value" class:placeholder={!issLocation.data}>{issLocation.data ? formatCoord(issLocation.data.longitude, false) : "---.----"}</span>
          </div>
          <div>
            <span class="label">Time</span>
            <span class="value" class:placeholder={!issLocation.data}>{issLocation.data ? formatTime(issLocation.data.api_timestamp) : "--:--:--"}</span>
          </div>
        </div>
        {#if issLocation.data}
          <p class="muted small">Updated every minute via cron</p>
        {:else}
          <p class="muted small">Waiting for first cron run...</p>
        {/if}
      </section>

      <section class="card">
        <h2>Cached Query <span class="badge">cache = 10s</span></h2>
        <p class="muted small cache-desc">Server-side query takes ~500ms (simulated). Cache returns instantly.</p>
        <button onclick={handleFetchStats} disabled={cacheLoading}>
          {cacheLoading ? "Fetching..." : "Fetch Stats"}
        </button>
        {#if cacheData}
          <div class="cache-stats">
            <div class="stat-row"><span class="meta-key">Users</span><span class="mono">{cacheData.total_users}</span></div>
            <div class="stat-row"><span class="meta-key">Trades</span><span class="mono">{cacheData.total_trades}</span></div>
            <div class="stat-row"><span class="meta-key">Webhooks</span><span class="mono">{cacheData.total_webhooks}</span></div>
            <div class="stat-row"><span class="meta-key">Computed</span><span class="mono">{formatTime(cacheData.computed_at)}</span></div>
          </div>
        {/if}
        {#if responseMs !== null}
          <p class="hint {responseMs < 100 ? 'success' : 'warning'}" style="margin-top: 0.5rem;">
            {responseMs.toFixed(0)}ms {responseMs < 100 ? "· cache hit" : "· cache miss"} · fetch #{fetchCount}
          </p>
        {/if}
      </section>

      <section class="card">
        <h2>Export Job <span class="badge">job</span></h2>
        {#if exportJobStore && $exportJobStore}
          <div class="progress-bar"><div class="fill" style="width:{$exportJobStore.progress || 0}%"></div></div>
          <p class="progress-text">{$exportJobStore.progress || 0}% - {$exportJobStore.message || $exportJobStore.status}</p>
          {#if ["completed", "failed", "pending"].includes($exportJobStore.status)}
            <button onclick={startExportJob}>Run Again</button>
          {/if}
        {:else}
          <p class="muted small export-desc">Ready to export users to CSV</p>
          <button onclick={startExportJob}>Start Export</button>
        {/if}
      </section>

      <section class="card mcp-card">
        <h2>MCP Tools <span class="badge green">model context protocol</span></h2>
        <p class="mcp-desc">This demo exposes MCP tools with OAuth 2.1 authentication. AI assistants authenticate via browser login and can act on behalf of the user.</p>
        <div class="code-block">
          <div class="code-label">CLAUDE CODE</div>
          <pre><code>claude mcp add forge-demo --transport http {apiUrl}/_api/mcp</code></pre>
        </div>
        <div class="mcp-tools">
          <div class="mcp-tool">
            <div class="tool-header">
              <span class="tool-name mono">demo.me</span>
              <span class="tool-badge">authenticated</span>
            </div>
            <span class="tool-desc">Get your own profile (requires OAuth login)</span>
          </div>
          <div class="mcp-tool">
            <div class="tool-header">
              <span class="tool-name mono">demo.list_users</span>
              <span class="tool-badge">public</span>
            </div>
            <span class="tool-desc">List all users with their roles</span>
          </div>
          <div class="mcp-tool">
            <div class="tool-header">
              <span class="tool-name mono">demo.get_user_by_email</span>
              <span class="tool-badge">public</span>
            </div>
            <span class="tool-desc">Look up a single user by email address</span>
          </div>
        </div>
      </section>
    </div>

    <!-- Right column -->
    <div class="col">
      <section class="card">
        <h2>Live Trades <span class="badge green">daemon + websocket</span></h2>
        <table class="trades-table">
          <thead>
            <tr><th>Symbol</th><th>Price</th><th>Qty</th><th>Side</th></tr>
          </thead>
          <tbody>
            {#if trades.data && trades.data.length > 0}
              {#each trades.data as trade (trade.id)}
                <tr>
                  <td class="mono">{trade.symbol}</td>
                  <td class="mono">{formatPrice(trade.price)}</td>
                  <td class="mono">{trade.quantity.toFixed(4)}</td>
                  <td class={trade.is_buyer_maker ? "sell" : "buy"}>{trade.is_buyer_maker ? "SELL" : "BUY"}</td>
                </tr>
              {/each}
            {:else}
              {#each [0, 1, 2, 3] as i (i)}
                <tr class="placeholder-row">
                  <td class="mono">---</td>
                  <td class="mono">-.-----</td>
                  <td class="mono">-.----</td>
                  <td>-</td>
                </tr>
              {/each}
            {/if}
          </tbody>
        </table>
        {#if trades.data && trades.data.length > 0}
          <p class="muted small">Streaming from Binance EUR/USDT</p>
        {:else}
          <p class="muted small">Connecting to Binance WebSocket...</p>
        {/if}
      </section>

      <section class="card">
        <h2>Auth <span class="badge purple">refresh tokens</span></h2>
        {#if auth.isAuthenticated}
          <div class="auth-user">
            <span class="label">Logged in as</span>
            {#if auth.user}
              <span class="value">{auth.user.name} ({auth.user.email})</span>
            {/if}
          </div>
          <div class="input-label" style="margin-top: 0.5rem;">TOKEN METADATA</div>
          <div class="token-meta">
            {#if tokenClaims}
              {#each tokenClaims as [key, val] (key)}
                <div class="meta-row">
                  <span class="meta-key">{key}</span>
                  <span class="mono">{val}</span>
                </div>
              {/each}
            {/if}
          </div>
          <div class="auth-actions">
            <button onclick={handleRefresh}>Refresh Token</button>
            <button class="secondary" onclick={handleLogout}>Logout</button>
          </div>
          {#if refreshCount > 0}
            <p class="hint success">Token refreshed {refreshCount} time{refreshCount > 1 ? "s" : ""}</p>
          {/if}
        {:else}
          <div class="auth-tabs">
            <button class="tab" class:active={authMode === "login"} onclick={() => { authMode = "login"; signals.track("auth_tab_switch", { tab: "login" }); }}>Login</button>
            <button class="tab" class:active={authMode === "register"} onclick={() => { authMode = "register"; signals.track("auth_tab_switch", { tab: "register" }); }}>Register</button>
          </div>
          <form onsubmit={handleAuth}>
            {#if authMode === "register"}
              <input type="text" placeholder="Name" bind:value={authName} />
            {/if}
            <input type="email" placeholder="Email" bind:value={authEmail} />
            <input type="password" placeholder="Password (min 8 chars)" bind:value={authPassword} />
            <button type="submit" disabled={authLoading}>
              {authLoading ? "..." : authMode === "login" ? "Login" : "Register"}
            </button>
          </form>
          <p class="muted small">Try demo@example.com / password123</p>
        {/if}
        {#if authError}
          <p class="hint warning">{authError}</p>
        {/if}
      </section>

      <section class="card">
        <h2>Webhook <span class="badge">webhook</span></h2>
        <label class="input-label" for="idempotency-key">Idempotency Key</label>
        <div class="webhook-row">
          <input id="idempotency-key" type="text" class="key-input" class:used={keyUsed} value={idempotencyKey} readonly />
          <button class="small" onclick={newKey}>New</button>
          <button disabled={keyUsed} onclick={triggerWebhook}>Send</button>
        </div>
        {#if keyUsed}
          <p class="hint success">Webhook processed. Generate a new key to send another.</p>
        {/if}
        {#if webhookError}
          <p class="hint warning">{webhookError}</p>
        {/if}
        {#if webhookEvents.data && webhookEvents.data.length > 0}
          <span class="input-label events-label">Recent Events</span>
          <div class="events">
            {#each webhookEvents.data as ev (ev.id)}
              <div class="event">
                <span class="mono">{ev.idempotency_key}</span>
                <span class="time">{formatTime(ev.processed_at)}</span>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <section class="card">
        <h2>Verification <span class="badge purple">workflow</span></h2>
        {#if workflowStore && $workflowStore}
          <div class="steps">
            {#each $workflowStore.steps as step (step.name)}
              <div class="step {step.status}">
                <span class="icon">{stepIcon(step.status)}</span>
                <span>{step.name}</span>
              </div>
            {/each}
          </div>
          {#if $workflowStore.status === "waiting" || ($workflowStore.status === "running" && confirmSent)}
            <p class="muted small">{confirmSent ? "Confirmation sent, finishing up..." : "Waiting for your confirmation..."}</p>
            <button class="confirm-btn" onclick={handleConfirmVerification} disabled={confirmSent}>
              {confirmSent ? "Confirmed" : "Confirm Verification"}
            </button>
          {:else if ["completed", "failed"].includes($workflowStore.status)}
            <button onclick={startVerificationWorkflow}>Run Again</button>
          {/if}
        {:else}
          <p class="muted small workflow-desc">Multi-step workflow with event wait</p>
          <button onclick={startVerificationWorkflow}>Start Workflow</button>
        {/if}
      </section>
    </div>
  </div>

  <section class="card">
    <h2>Users <span class="badge green">crud + subscribe</span></h2>
    <form class="form-row" onsubmit={handleCreateUser}>
      <input type="text" placeholder="Name" bind:value={name} required />
      <input type="email" placeholder="Email" bind:value={email} required />
      <button type="submit" disabled={isSubmitting}>{isSubmitting ? "..." : "Create"}</button>
    </form>
    {#if crudError}
      <p class="hint warning">{crudError}</p>
    {/if}
    {#if users.data && users.data.length > 0}
      <div class="table-wrap">
        <table>
          <thead><tr><th>Name</th><th>Email</th><th></th></tr></thead>
          <tbody>
            {#each users.data as user (user.id)}
              {#if editingUserId === user.id}
                <tr class="editing">
                  <td><input type="text" bind:value={editName} /></td>
                  <td><input type="email" bind:value={editEmail} /></td>
                  <td>
                    <button class="small" onclick={handleUpdateUser} disabled={isEditing}>Save</button>
                    <button class="small secondary" onclick={cancelEdit}>Cancel</button>
                  </td>
                </tr>
              {:else}
                <tr>
                  <td>{user.name}</td>
                  <td>{user.email}</td>
                  <td>
                    <div class="action-cell">
                      <button class="small" onclick={() => startEditUser(user)}>Edit</button>
                      <button class="small danger" onclick={() => deletePopoverUserId = user.id}>Delete</button>
                      {#if deletePopoverUserId === user.id}
                        <div class="popover">
                          <button class="small danger" onclick={() => confirmDeleteUser(user.id)}>Confirm</button>
                          <button class="small" onclick={() => deletePopoverUserId = null}>Cancel</button>
                        </div>
                      {/if}
                    </div>
                  </td>
                </tr>
              {/if}
            {/each}
          </tbody>
        </table>
      </div>
    {:else if !users.loading}
      <p class="muted">No users yet. Create one above.</p>
    {/if}
  </section>
</main>

<style>
  /* Reset & base */
  main {
    max-width: 80rem;
    margin: 0 auto;
    padding: 2rem;
    font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    line-height: 1.5;
  }

  h2 { margin: 0 0 0.75rem; font-size: 0.95rem; font-weight: 600; }

  /* Layout */
  .columns { display: flex; gap: 1rem; align-items: flex-start; margin-bottom: 1rem; }
  .col { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1rem; }

  /* Cards */
  .card { background: #fff; border: 1px solid #e2e8f0; border-radius: 10px; padding: 1.25rem; box-shadow: 0 1px 2px rgb(0 0 0 / 4%); }
  .card.dark { background: linear-gradient(135deg, #0f172a, #1e293b); border-color: #334155; color: #f1f5f9; }
  .card.dark .muted { color: #64748b; }

  /* Badges */
  .badge { display: inline-block; margin-left: 0.4rem; padding: 0.1rem 0.35rem; border-radius: 3px; background: #e5e5e5; color: #666; font-size: 0.6rem; font-weight: 500; text-transform: uppercase; letter-spacing: 0.02em; vertical-align: middle; }
  .badge.green { background: #dcfce7; color: #166534; }
  .badge.purple { background: #ede9fe; color: #5b21b6; }

  /* ISS / stat values */
  .stats { display: flex; gap: 1.5rem; }
  .stats .label { display: block; color: #94a3b8; font-size: 0.7rem; text-transform: uppercase; letter-spacing: 0.04em; }
  .stats .value { color: #60a5fa; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 1rem; font-weight: 600; white-space: nowrap; }
  .stats .value.placeholder { color: #475569; }

  /* Text helpers */
  .muted { margin: 0; color: #999; font-style: italic; }
  .muted.small { margin-top: 0.5rem; font-size: 0.8rem; }
  .workflow-desc, .export-desc { margin-bottom: 0.75rem; }
  .cache-desc { margin-bottom: 0.75rem; margin-top: 0; }
  .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }

  /* Trades table */
  .trades-table, table { width: 100%; border-collapse: collapse; }
  .trades-table th { padding: 0.4rem 0; border-bottom: 1px solid #eee; color: #666; font-size: 0.8rem; font-weight: 500; text-align: left; }
  .trades-table td { padding: 0.4rem 0; border-bottom: 1px solid #f5f5f5; font-size: 0.8rem; }
  :global(.buy) { color: #16a34a; font-weight: 500; }
  :global(.sell) { color: #dc2626; font-weight: 500; }
  .placeholder-row { color: #ccc; }

  /* Input labels */
  .input-label { display: block; margin-bottom: 0.3rem; color: #666; font-size: 0.7rem; font-weight: 500; letter-spacing: 0.05em; text-transform: uppercase; }
  .events-label { margin-top: 0.75rem; }

  /* Webhook */
  .webhook-row, .form-row { display: flex; gap: 0.5rem; }
  .webhook-row { margin-bottom: 0.5rem; }
  .key-input, input { flex: 1; min-width: 0; padding: 0.4rem 0.6rem; border: 1px solid #ccc; border-radius: 4px; background: #fff; color: #111827; font-family: inherit; font-size: 0.85rem; }
  .key-input { background: #fafafa; border-color: #d1d5db; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.8rem; }
  .key-input.used { background: #f0fdf4; border-color: #16a34a; border-width: 2px; color: #166534; }
  .hint { margin: 0 0 0.5rem; font-size: 0.75rem; }
  .hint.warning { color: #b45309; }
  .hint.success { color: #166534; }
  .events { display: flex; flex-direction: column; gap: 0.3rem; }
  .event { display: flex; justify-content: space-between; gap: 0.5rem; padding: 0.4rem 0.6rem; border: 1px solid #e2e8f0; border-radius: 5px; background: #f8fafc; font-size: 0.8rem; }
  .event .time { color: #64748b; }

  /* Progress / Jobs */
  .progress-bar { height: 6px; margin-bottom: 0.5rem; border-radius: 3px; background: #e5e5e5; overflow: hidden; }
  .fill { height: 100%; background: #0066cc; transition: width 0.3s; }
  .progress-text { margin: 0 0 0.5rem; color: #666; font-size: 0.8rem; }

  /* Workflow steps */
  .steps { display: flex; flex-direction: column; gap: 0.3rem; margin-bottom: 0.75rem; }
  .step { display: flex; align-items: center; gap: 0.4rem; padding: 0.35rem 0.5rem; border-radius: 4px; background: #f5f5f5; font-size: 0.8rem; }
  .step.completed { background: #dcfce7; }
  .step.running { background: #dbeafe; }
  .step.failed { background: #fee2e2; }
  .icon { width: 1.2rem; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.75rem; text-align: center; }

  /* Buttons */
  button { padding: 0.4rem 0.8rem; border: none; border-radius: 4px; background: #0066cc; color: #fff; cursor: pointer; font-family: inherit; font-size: 0.85rem; }
  button:hover:not(:disabled) { background: #0055aa; }
  button:disabled { background: #999; cursor: not-allowed; }
  button.small { padding: 0.2rem 0.4rem; font-size: 0.8rem; }
  button.secondary { background: #666; }
  button.danger { background: #dc2626; }

  /* Users table */
  .form-row { margin-bottom: 1rem; }
  th, td { padding: 0.6rem; border-bottom: 1px solid #eee; text-align: left; }
  th { color: #555; font-weight: 600; font-size: 0.85rem; }
  .editing { background: #f0f9ff; }
  .table-wrap { overflow-x: auto; }
  .action-cell { position: relative; display: inline-flex; gap: 0.5rem; white-space: nowrap; }
  .popover { position: absolute; top: 100%; right: 0; z-index: 100; display: flex; gap: 0.5rem; margin-top: 4px; padding: 0.5rem; border: 1px solid #e0e0e0; border-radius: 6px; background: #fff; box-shadow: 0 4px 12px rgb(0 0 0 / 15%); }

  /* Auth card */
  .auth-tabs { display: flex; gap: 0; margin-bottom: 0.75rem; }
  .tab { flex: 1; padding: 0.35rem; background: #f5f5f5; border: 1px solid #e2e8f0; color: #555; font-size: 0.8rem; cursor: pointer; }
  .tab:hover:not(.active) { background: #e2e8f0; color: #1e293b; }
  .tab:first-child { border-radius: 5px 0 0 5px; }
  .tab:last-child { border-radius: 0 5px 5px 0; }
  .tab.active { background: #0066cc; color: white; border-color: #0066cc; }
  .card form { display: flex; flex-direction: column; gap: 0.5rem; }
  .card form input { flex: unset; width: 100%; box-sizing: border-box; }
  .card form button[type="submit"] { align-self: flex-start; }
  .auth-user { margin-bottom: 0.5rem; }
  .auth-user .label { font-size: 0.7rem; color: #94a3b8; display: block; }
  .auth-user .value { font-size: 0.9rem; font-weight: 500; color: #111827; }
  .token-meta { display: flex; flex-direction: column; gap: 0.2rem; margin-bottom: 0.6rem; }
  .meta-row { display: flex; justify-content: space-between; font-size: 0.75rem; padding: 0.3rem 0.5rem; background: #f8fafc; border: 1px solid #e2e8f0; border-radius: 4px; }
  .meta-key { font-weight: 500; color: #666; min-width: 2.5rem; }
  .auth-actions { display: flex; gap: 0.5rem; }

  /* Cache card */
  .cache-stats { margin-top: 0.75rem; display: flex; flex-direction: column; gap: 0.2rem; }
  .stat-row { display: flex; justify-content: space-between; font-size: 0.8rem; padding: 0.3rem 0.5rem; background: #f8fafc; border: 1px solid #e2e8f0; border-radius: 4px; }

  /* MCP card */
  .mcp-desc { font-size: 0.85rem; color: #555; margin: 0 0 0.75rem; }
  .code-block { background: #0f172a; border-radius: 8px; padding: 0.75rem 1rem; margin-bottom: 1rem; }
  .code-label { font-size: 0.65rem; font-weight: 500; color: #64748b; letter-spacing: 0.05em; text-transform: uppercase; margin-bottom: 0.3rem; }
  .code-block pre { margin: 0; overflow-x: auto; }
  .code-block code { color: #e2e8f0; font-size: 0.8rem; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap; word-break: break-all; }
  .mcp-tools { display: flex; flex-direction: column; gap: 0.5rem; }
  .mcp-tool { display: flex; flex-direction: column; gap: 0.2rem; font-size: 0.8rem; padding: 0.6rem 0.75rem; background: #f8fafc; border: 1px solid #e2e8f0; border-radius: 6px; }
  .tool-header { display: flex; align-items: center; justify-content: space-between; gap: 0.5rem; }
  .tool-name { font-weight: 500; }
  .tool-desc { color: #666; font-size: 0.75rem; }
  .tool-badge { font-size: 0.6rem; padding: 0.1rem 0.3rem; border-radius: 3px; background: #dcfce7; color: #166534; text-transform: uppercase; white-space: nowrap; }

  /* Responsive */
  @media (max-width: 700px) {
    main { padding: 1rem; }
    .columns { flex-direction: column; }
    .col { width: 100%; }
    .card { padding: 1rem; }
    .stats { gap: 1rem; flex-wrap: wrap; }
    .value { font-size: 0.95rem; }
    .webhook-row { flex-wrap: wrap; }
    .key-input { flex: 1 1 100%; font-size: 0.8rem; }
    .form-row { flex-wrap: wrap; }
    .form-row input, .form-row button { flex: 1 1 100%; }
    table { font-size: 0.85rem; }
    th, td { padding: 0.5rem 0.25rem; }
    .action-cell { display: flex; flex-wrap: wrap; gap: 0.25rem; }
    .popover { position: fixed; top: 50%; right: auto; left: 50%; transform: translate(-50%, -50%); }
    .trades-table { font-size: 0.8rem; }
    .code-block code { font-size: 0.7rem; }
  }
</style>
