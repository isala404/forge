<script lang="ts">
	import { ForgeProvider } from '@forge/svelte';
	import '../app.css';

	let { children } = $props();
</script>

<ForgeProvider url="http://localhost:8080">
	<div class="app">
		<nav class="sidebar">
			<div class="logo">
				<span class="logo-icon">📋</span>
				<span class="logo-text">Task Manager</span>
			</div>
			<ul class="nav-items">
				<li><a href="/" class="nav-link active">Board</a></li>
				<li><a href="/settings" class="nav-link">Settings</a></li>
			</ul>
		</nav>
		<main class="content">
			{@render children()}
		</main>
	</div>
</ForgeProvider>

<style>
	.app {
		display: flex;
		height: 100vh;
	}

	.sidebar {
		width: 240px;
		background: var(--color-surface);
		border-right: 1px solid var(--color-border);
		padding: 1rem;
		display: flex;
		flex-direction: column;
	}

	.logo {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.5rem;
		margin-bottom: 1.5rem;
	}

	.logo-icon {
		font-size: 1.5rem;
	}

	.logo-text {
		font-weight: 600;
		font-size: 1.125rem;
	}

	.nav-items {
		list-style: none;
	}

	.nav-link {
		display: block;
		padding: 0.5rem 0.75rem;
		border-radius: var(--radius-sm);
		color: var(--color-text-muted);
		text-decoration: none;
		transition: all 0.15s;
	}

	.nav-link:hover, .nav-link.active {
		background: var(--color-surface-hover);
		color: var(--color-text);
	}

	.content {
		flex: 1;
		overflow: auto;
		padding: 1.5rem;
	}
</style>
