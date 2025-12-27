<script lang="ts">
	import { subscribe, action } from '@forge/svelte';
	import { getTeam, exportProjectCsv } from '$lib/forge/api';

	// Get team info
	const teamId = '10000000-0000-0000-0000-000000000001';
	const team = subscribe(getTeam, { teamId });

	let exportLoading = $state(false);
	let exportResult = $state<{ downloadUrl: string } | null>(null);

	async function handleExport() {
		exportLoading = true;
		try {
			const result = await action(exportProjectCsv, { projectId: '20000000-0000-0000-0000-000000000001' });
			exportResult = result;
		} finally {
			exportLoading = false;
		}
	}
</script>

<h1>Settings</h1>

<div class="settings-section">
	<h2>Team Information</h2>
	{#if $team.loading}
		<p class="muted">Loading...</p>
	{:else if $team.data}
		<div class="info-grid">
			<div class="info-item">
				<label>Name</label>
				<span>{$team.data.name}</span>
			</div>
			<div class="info-item">
				<label>Slug</label>
				<span>{$team.data.slug}</span>
			</div>
			<div class="info-item">
				<label>Description</label>
				<span>{$team.data.description || 'No description'}</span>
			</div>
		</div>
	{/if}
</div>

<div class="settings-section">
	<h2>Export Data</h2>
	<p class="muted">Export project tasks to CSV format.</p>
	<button class="primary" onclick={handleExport} disabled={exportLoading}>
		{exportLoading ? 'Exporting...' : 'Export to CSV'}
	</button>
	{#if exportResult}
		<p class="success">Export ready: <a href={exportResult.downloadUrl}>Download</a></p>
	{/if}
</div>

<style>
	h1 {
		font-size: 1.5rem;
		font-weight: 600;
		margin-bottom: 1.5rem;
	}

	.settings-section {
		background: var(--color-surface);
		border-radius: var(--radius-md);
		padding: 1.5rem;
		margin-bottom: 1rem;
	}

	.settings-section h2 {
		font-size: 1rem;
		font-weight: 600;
		margin-bottom: 1rem;
	}

	.muted {
		color: var(--color-text-muted);
		font-size: 0.875rem;
		margin-bottom: 1rem;
	}

	.info-grid {
		display: grid;
		gap: 1rem;
	}

	.info-item {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.info-item label {
		font-size: 0.75rem;
		color: var(--color-text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.success {
		color: var(--color-success);
		margin-top: 1rem;
	}

	.success a {
		color: var(--color-primary);
	}
</style>
