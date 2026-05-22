<script lang="ts">
  import { getForgeSignals } from "@forge-rs/svelte";
  import { PUBLIC_API_URL } from "$env/static/public";
  const signals = getForgeSignals();
  const apiUrl = PUBLIC_API_URL;
</script>

<h1>minimal</h1>
<p class="subtitle">
  Backend: <a href={apiUrl + "/health"} target="_blank" rel="external noopener" onclick={() => signals.track('link_clicked', { target: 'health_check', url: apiUrl + '/health' })}>{apiUrl}</a>
</p>

<section class="card">
  <h2>Getting Started</h2>
  <p>Your FORGE project is ready! Here's what you can do:</p>
  <ul>
    <li>Define models in <code>src/schema/</code></li>
    <li>Create queries and mutations in <code>src/functions/</code></li>
    <li>Register them in <code>src/main.rs</code></li>
    <li>Add migrations in <code>migrations/</code></li>
  </ul>
  <p>
    Check the <a href="https://tryforge.dev/docs" target="_blank" onclick={() => signals.track('link_clicked', { target: 'documentation', url: 'https://tryforge.dev/docs' })}>documentation</a> for guides and examples.
  </p>
</section>

<style>
  h1 {
    margin: 0;
  }

  h2 {
    margin: 0 0 0.75rem;
    font-size: 1.1rem;
  }

  .subtitle {
    margin: -0.5rem 0 1.5rem;
    color: #666;
  }

  .subtitle a {
    color: #0066cc;
  }

  .card {
    background: #fff;
    border: 1px solid #e0e0e0;
    border-radius: 8px;
    padding: 1.25rem;
  }

  ul {
    margin: 0.5rem 0;
    padding-left: 1.5rem;
  }

  li {
    margin: 0.25rem 0;
  }

  code {
    background: #f5f5f5;
    padding: 0.125rem 0.375rem;
    border-radius: 3px;
    font-size: 0.9em;
  }

  a {
    color: #0066cc;
  }
</style>
