<script lang="ts">
  import {
    listTodos$,
    createTodo,
    updateTodo,
    deleteTodo,
    register,
    login,
  } from "$lib/forge";
  import type { ForgeError } from "$lib/forge";
  import { auth } from "$lib/forge/auth.svelte";
  import { getForgeSignals } from "@forge-rs/svelte";

  const signals = getForgeSignals();

  let mode: "login" | "register" = $state("login");
  let email = $state("");
  let name = $state("");
  let password = $state("");
  let authError: string | null = $state(null);
  let authSubmitting = $state(false);

  let newTitle: string = $state("");
  let error: ForgeError | null = $state(null);
  let adding: boolean = $state(false);

  const isAuthed = $derived(auth.isAuthenticated);

  async function handleAuth(e: Event) {
    e.preventDefault();
    authError = null;
    authSubmitting = true;
    try {
      const res =
        mode === "register"
          ? await register({ email, name, password })
          : await login({ email, password });
      auth.setAuth(res.access_token, res.refresh_token, {
        id: res.user.id,
        email: res.user.email,
        name: res.user.name,
      });
      email = "";
      name = "";
      password = "";
    } catch (err) {
      authError = (err as ForgeError).message;
    } finally {
      authSubmitting = false;
    }
  }

  function handleLogout() {
    signals.breadcrumb("Logout");
    auth.clearAuth();
  }
</script>

<main>
  <div class="shell">
    <header class="hero">
      <h1>Todos</h1>
      {#if isAuthed}
        <div class="user-row">
          <span class="user">{auth.user?.name ?? auth.user?.email}</span>
          <button class="logout" onclick={handleLogout}>Sign out</button>
        </div>
      {/if}
    </header>

    {#if !isAuthed}
      <section class="auth-panel">
        <div class="tabs">
          <button
            class:active={mode === "login"}
            onclick={() => (mode = "login")}>Sign in</button
          >
          <button
            class:active={mode === "register"}
            onclick={() => (mode = "register")}>Sign up</button
          >
        </div>
        <form onsubmit={handleAuth}>
          {#if mode === "register"}
            <input
              type="text"
              placeholder="Name"
              bind:value={name}
              required
            />
          {/if}
          <input
            type="email"
            placeholder="Email"
            bind:value={email}
            required
          />
          <input
            type="password"
            placeholder="Password (min 8 chars)"
            bind:value={password}
            minlength="8"
            required
          />
          <button type="submit" disabled={authSubmitting}>
            {authSubmitting ? "..." : mode === "login" ? "Sign in" : "Sign up"}
          </button>
        </form>
        {#if authError}
          <p class="error">{authError}</p>
        {/if}
      </section>
    {:else}
      {@const todos = listTodos$()}
      {@const remainingCount =
        todos.data?.filter((t) => !t.completed).length ?? 0}

      <section class="input-panel">
        <div class="input-row">
          <input
            type="text"
            placeholder="What needs to be done?"
            bind:value={newTitle}
            onkeydown={(e) => {
              if (e.key === "Enter") {
                (async () => {
                  if (!newTitle.trim()) return;
                  adding = true;
                  error = null;
                  try {
                    await createTodo({ title: newTitle.trim() });
                    newTitle = "";
                  } catch (e) {
                    error = e as ForgeError;
                  } finally {
                    adding = false;
                  }
                })();
              }
            }}
            disabled={adding}
          />
          <button
            onclick={async () => {
              if (!newTitle.trim()) return;
              adding = true;
              error = null;
              try {
                await createTodo({ title: newTitle.trim() });
                newTitle = "";
              } catch (e) {
                error = e as ForgeError;
              } finally {
                adding = false;
              }
            }}
            disabled={adding || !newTitle.trim()}
          >
            {adding ? "Adding..." : "Add"}
          </button>
        </div>

        {#if error}
          <p class="error">{error.message}</p>
        {/if}
      </section>

      <section class="list-panel">
        {#if todos.data && todos.data.length > 0}
          <div class="list-head">
            <span class="summary">{remainingCount} remaining</span>
          </div>
        {/if}

        {#if todos.loading}
          <p class="status">Loading...</p>
        {:else if todos.error}
          <p class="error">{todos.error.message}</p>
        {:else if todos.data}
          {#if todos.data.length === 0}
            <p class="status">No todos yet. Add one above!</p>
          {:else}
            <ul>
              {#each todos.data as todo (todo.id)}
                <li class:completed={todo.completed}>
                  <label>
                    <input
                      type="checkbox"
                      checked={todo.completed}
                      onchange={async () => {
                        try {
                          await updateTodo({
                            id: todo.id,
                            completed: !todo.completed,
                          });
                        } catch (e) {
                          error = e as ForgeError;
                        }
                      }}
                    />
                    <span class="title">{todo.title}</span>
                  </label>
                  <button
                    class="delete"
                    onclick={async () => {
                      try {
                        await deleteTodo({ id: todo.id });
                      } catch (e) {
                        error = e as ForgeError;
                      }
                    }}
                  >
                    Delete
                  </button>
                </li>
              {/each}
            </ul>
            <p class="count">
              {remainingCount} remaining
            </p>
          {/if}
        {/if}
      </section>
    {/if}
  </div>
</main>

<style>
  :global(body) {
    margin: 0;
    background: #fff;
    color: #222;
    font-family:
      -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    min-height: 100vh;
  }

  main {
    max-width: 480px;
    margin: 0 auto;
    padding: 0 1rem;
  }

  .shell {
    padding: 2rem 0 3rem;
  }

  .hero {
    padding: 0 0 0.75rem;
    border-bottom: 1px solid #e5e5e5;
    margin-bottom: 1rem;
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  h1 {
    margin: 0;
    font-weight: 600;
    font-size: 1.5rem;
    color: #111;
  }

  .user-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    font-size: 0.85rem;
  }

  .user {
    color: #555;
  }

  .logout {
    padding: 0.3rem 0.6rem;
    font-size: 0.78rem;
    background: #fff;
    color: #555;
    border: 1px solid #ddd;
    border-radius: 4px;
    cursor: pointer;
    font-family: inherit;
  }

  .logout:hover {
    background: #f5f5f5;
  }

  .auth-panel {
    margin-bottom: 1.5rem;
  }

  .tabs {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
  }

  .tabs button {
    flex: 1;
    padding: 0.4rem;
    font-size: 0.85rem;
    background: #f7f7f7;
    color: #444;
    border: 1px solid #e0e0e0;
    border-radius: 4px;
    cursor: pointer;
    font-family: inherit;
  }

  .tabs button.active {
    background: #111;
    color: #fff;
    border-color: #111;
  }

  .auth-panel form {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .auth-panel input,
  .auth-panel button[type="submit"] {
    padding: 0.5rem 0.75rem;
    font-size: 0.9rem;
    border-radius: 4px;
    font-family: inherit;
  }

  .auth-panel input {
    border: 1px solid #ccc;
    background: #fff;
    color: #222;
  }

  .auth-panel input:focus {
    border-color: #888;
    outline: none;
  }

  .auth-panel button[type="submit"] {
    background: #111;
    color: #fff;
    border: none;
    cursor: pointer;
    font-weight: 500;
  }

  .auth-panel button[type="submit"]:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .input-panel {
    margin-bottom: 1.5rem;
  }

  .input-row {
    display: flex;
    gap: 0.5rem;
  }

  .input-row input {
    flex: 1;
    padding: 0.5rem 0.75rem;
    font-size: 0.9rem;
    border: 1px solid #ccc;
    border-radius: 4px;
    background: #fff;
    color: #222;
    font-family: inherit;
    outline: none;
  }

  .input-row input:focus {
    border-color: #888;
  }

  .input-row button {
    padding: 0.5rem 1rem;
    font-size: 0.9rem;
    background: #111;
    color: #fff;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-weight: 500;
    font-family: inherit;
    white-space: nowrap;
  }

  .input-row button:hover {
    background: #333;
  }

  .input-row button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .list-head {
    display: flex;
    justify-content: flex-end;
    margin-bottom: 0.5rem;
  }

  .summary {
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #888;
  }

  ul {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.6rem 0;
    border-bottom: 1px solid #eee;
  }

  li:last-child {
    border-bottom: none;
  }

  li label {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex: 1;
    cursor: pointer;
  }

  li label input[type="checkbox"] {
    width: 1rem;
    height: 1rem;
    margin: 0;
    cursor: pointer;
    flex-shrink: 0;
    accent-color: #111;
  }

  .title {
    font-size: 0.9rem;
  }

  li.completed .title {
    text-decoration: line-through;
    color: #999;
  }

  .delete {
    background: none;
    border: 1px solid transparent;
    color: #c44;
    cursor: pointer;
    padding: 0.2rem 0.5rem;
    font-size: 0.78rem;
    font-family: inherit;
    border-radius: 3px;
    opacity: 0;
  }

  li:hover .delete {
    opacity: 1;
  }

  .delete:hover {
    background: #fef0f0;
    border-color: #e8c0c0;
  }

  .status {
    color: #888;
    text-align: center;
    padding: 1.5rem 1rem;
    font-size: 0.88rem;
  }

  .error {
    color: #b91c1c;
    padding: 0.5rem 0.7rem;
    background: #fef2f2;
    border: 1px solid #fecaca;
    border-radius: 4px;
    margin-top: 0.5rem;
    font-size: 0.85rem;
  }

  .count {
    color: #888;
    text-align: center;
    font-size: 0.8rem;
    margin-top: 0.5rem;
    padding-top: 0.5rem;
  }
</style>
