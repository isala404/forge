<script>
  import axios from 'axios';
  import { onMount } from 'svelte';

  let links = $state([]);
  let total = $state(0);
  let url = $state('');
  let result = $state('');

  async function refresh() {
    try {
      const [l, s] = await Promise.all([axios.get('/api/links'), axios.get('/api/stats')]);
      links = l.data;
      total = s.data.total_clicks;
    } catch {
      /* transient; the interval retries */
    }
  }

  async function submit(event) {
    event.preventDefault();
    try {
      const { data } = await axios.post('/api/links', { url });
      result = `${location.origin}${data.short_url}`;
      url = '';
      refresh();
    } catch (err) {
      const msg =
        err?.response?.data?.message || err?.response?.data || 'Could not shorten that URL';
      alert(msg);
    }
  }

  onMount(() => {
    refresh();
    const t = setInterval(refresh, 1500);
    return () => clearInterval(t);
  });
</script>

<svelte:head><title>Linkly (SvelteKit) — on Forge</title></svelte:head>

<main>
  <h1>🔗 Linkly <span class="badge">SvelteKit + napi</span></h1>
  <p class="sub">
    A URL shortener whose backend is <strong>Forge</strong>, called from SvelteKit through the Node
    bindings — <code>kv</code> for storage, <code>queue</code> for async click analytics.
  </p>

  <form onsubmit={submit}>
    <input
      id="url"
      type="url"
      placeholder="https://example.com/a/very/long/link"
      bind:value={url}
      required
      autocomplete="off"
    />
    <button type="submit">Shorten</button>
  </form>
  {#if result}
    <div id="result" class="result">
      Created <a href={result} target="_blank" rel="noopener">{result}</a> &rarr; opens your link
    </div>
  {/if}

  <div class="stats">
    <span id="total">{total}</span><span>clicks processed (async, via the queue)</span>
  </div>

  <table>
    <thead><tr><th>Short link</th><th>Destination</th><th>Clicks</th></tr></thead>
    <tbody id="links">
      {#each links as l (l.code)}
        <tr>
          <td><a href={`/r/${l.code}`} target="_blank" rel="noopener">/r/{l.code}</a></td>
          <td class="dest" title={l.url}>{l.url}</td>
          <td class="clicks">{l.clicks}</td>
        </tr>
      {:else}
        <tr><td colspan="3" class="empty">No links yet — shorten one above.</td></tr>
      {/each}
    </tbody>
  </table>

  <footer>
    Click a short link to open it — the redirect enqueues an event onto Forge's queue that a JS
    worker (driving the binding's <code>dequeue</code>/<code>ack</code>) counts a beat later.
  </footer>
</main>

<style>
  :global(body) {
    margin: 0;
    font:
      15px/1.5 ui-sans-serif,
      system-ui,
      -apple-system,
      Segoe UI,
      Roboto,
      sans-serif;
    background: radial-gradient(1200px 600px at 50% -10%, #1b2030, #0f1117);
    color: #e7e9ee;
    min-height: 100vh;
  }
  main {
    max-width: 760px;
    margin: 0 auto;
    padding: 48px 20px 80px;
  }
  h1 {
    font-size: 28px;
    margin: 0 0 4px;
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .badge {
    font-size: 12px;
    font-weight: 600;
    color: #7ee787;
    border: 1px solid #2a3b2c;
    background: #14201600;
    padding: 3px 8px;
    border-radius: 999px;
  }
  .sub {
    color: #8b93a7;
    margin: 0 0 28px;
  }
  .sub strong {
    color: #7ee787;
  }
  form {
    display: flex;
    gap: 10px;
    margin-bottom: 14px;
  }
  input {
    flex: 1;
    padding: 12px 14px;
    border-radius: 10px;
    border: 1px solid #262b37;
    background: #181b24;
    color: #e7e9ee;
    font-size: 15px;
  }
  input:focus {
    outline: none;
    border-color: #6ea8fe;
  }
  button {
    padding: 12px 20px;
    border: 0;
    border-radius: 10px;
    cursor: pointer;
    background: #6ea8fe;
    color: #0a0d14;
    font-weight: 600;
    font-size: 15px;
  }
  button:hover {
    filter: brightness(1.08);
  }
  .result {
    background: #181b24;
    border: 1px solid #262b37;
    border-radius: 10px;
    padding: 12px 14px;
    margin-bottom: 22px;
    color: #7ee787;
  }
  .stats {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin: 26px 0 12px;
    color: #8b93a7;
  }
  .stats #total {
    font-size: 30px;
    font-weight: 700;
    color: #e7e9ee;
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: 11px 12px;
    border-bottom: 1px solid #262b37;
  }
  th {
    color: #8b93a7;
    font-weight: 600;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  td.dest {
    color: #8b93a7;
    max-width: 380px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  td.clicks {
    text-align: right;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  a {
    color: #6ea8fe;
    text-decoration: none;
  }
  a:hover {
    text-decoration: underline;
  }
  .empty {
    color: #8b93a7;
    text-align: center;
    padding: 26px;
  }
  footer {
    margin-top: 30px;
    color: #8b93a7;
    font-size: 13px;
  }
</style>
