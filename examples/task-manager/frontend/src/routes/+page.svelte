<script lang="ts">
	import { subscribe, mutate, mutateOptimisticUpdate } from '@forge/svelte';
	import { getProjectTasks, updateTask, createTask, type Task, type TaskStatus } from '$lib/forge/api';

	// Subscribe to tasks for the demo project (with real-time updates)
	const projectId = '20000000-0000-0000-0000-000000000001';
	const tasks = subscribe(getProjectTasks, { projectId });

	// Task columns configuration
	const columns: { status: TaskStatus; label: string; color: string }[] = [
		{ status: 'backlog', label: 'Backlog', color: '#6b7280' },
		{ status: 'todo', label: 'To Do', color: '#3b82f6' },
		{ status: 'in_progress', label: 'In Progress', color: '#f59e0b' },
		{ status: 'in_review', label: 'In Review', color: '#8b5cf6' },
		{ status: 'done', label: 'Done', color: '#10b981' },
	];

	// Filter tasks by column
	function getTasksForColumn(allTasks: Task[] | undefined, status: TaskStatus): Task[] {
		if (!allTasks) return [];
		return allTasks.filter(t => t.status === status).sort((a, b) => a.position - b.position);
	}

	// Priority badge colors
	function getPriorityColor(priority: string): string {
		switch (priority) {
			case 'urgent': return '#ef4444';
			case 'high': return '#f59e0b';
			case 'medium': return '#3b82f6';
			default: return '#6b7280';
		}
	}

	// Drag and drop state
	let draggedTask: Task | null = $state(null);

	function onDragStart(task: Task) {
		draggedTask = task;
	}

	function onDragOver(e: DragEvent) {
		e.preventDefault();
	}

	async function onDrop(targetStatus: TaskStatus) {
		if (!draggedTask || draggedTask.status === targetStatus) {
			draggedTask = null;
			return;
		}

		const taskId = draggedTask.id;
		const oldStatus = draggedTask.status;

		// Optimistic update: immediately move task to new column
		mutateOptimisticUpdate(
			tasks,
			(currentTasks) => currentTasks?.map(t =>
				t.id === taskId ? { ...t, status: targetStatus } : t
			),
			async () => {
				await mutate(updateTask, { taskId, status: targetStatus });
			}
		);

		draggedTask = null;
	}

	// Create new task
	let newTaskTitle = $state('');
	let showNewTaskInput = $state(false);

	async function handleCreateTask() {
		if (!newTaskTitle.trim()) return;

		await mutate(createTask, {
			projectId,
			title: newTaskTitle.trim(),
			priority: 'medium'
		});

		newTaskTitle = '';
		showNewTaskInput = false;
	}
</script>

<div class="board-header">
	<h1>Website Redesign</h1>
	<button class="primary" onclick={() => showNewTaskInput = true}>
		+ New Task
	</button>
</div>

{#if showNewTaskInput}
	<div class="new-task-form">
		<input
			type="text"
			placeholder="Task title..."
			bind:value={newTaskTitle}
			onkeydown={(e) => e.key === 'Enter' && handleCreateTask()}
		/>
		<button class="primary" onclick={handleCreateTask}>Add</button>
		<button class="secondary" onclick={() => showNewTaskInput = false}>Cancel</button>
	</div>
{/if}

{#if $tasks.loading}
	<div class="loading">Loading tasks...</div>
{:else if $tasks.error}
	<div class="error">Error: {$tasks.error.message}</div>
{:else}
	<div class="board">
		{#each columns as column}
			<div
				class="column"
				ondragover={onDragOver}
				ondrop={() => onDrop(column.status)}
			>
				<div class="column-header">
					<span class="column-dot" style="background: {column.color}"></span>
					<span class="column-label">{column.label}</span>
					<span class="column-count">{getTasksForColumn($tasks.data, column.status).length}</span>
				</div>
				<div class="column-tasks">
					{#each getTasksForColumn($tasks.data, column.status) as task (task.id)}
						<div
							class="task-card"
							draggable="true"
							ondragstart={() => onDragStart(task)}
						>
							<div class="task-title">{task.title}</div>
							{#if task.description}
								<div class="task-description">{task.description}</div>
							{/if}
							<div class="task-meta">
								<span class="priority-badge" style="background: {getPriorityColor(task.priority)}">
									{task.priority}
								</span>
								{#if task.dueDate}
									<span class="due-date">Due: {new Date(task.dueDate).toLocaleDateString()}</span>
								{/if}
							</div>
						</div>
					{/each}
				</div>
			</div>
		{/each}
	</div>
{/if}

<style>
	.board-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: 1.5rem;
	}

	.board-header h1 {
		font-size: 1.5rem;
		font-weight: 600;
	}

	.new-task-form {
		display: flex;
		gap: 0.5rem;
		margin-bottom: 1rem;
		padding: 1rem;
		background: var(--color-surface);
		border-radius: var(--radius-md);
	}

	.new-task-form input {
		flex: 1;
	}

	.loading, .error {
		padding: 2rem;
		text-align: center;
		color: var(--color-text-muted);
	}

	.error {
		color: var(--color-danger);
	}

	.board {
		display: flex;
		gap: 1rem;
		overflow-x: auto;
		padding-bottom: 1rem;
	}

	.column {
		flex: 0 0 280px;
		background: var(--color-surface);
		border-radius: var(--radius-md);
		padding: 0.75rem;
	}

	.column-header {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.5rem;
		margin-bottom: 0.5rem;
	}

	.column-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
	}

	.column-label {
		font-weight: 500;
		font-size: 0.875rem;
	}

	.column-count {
		margin-left: auto;
		font-size: 0.75rem;
		color: var(--color-text-muted);
		background: var(--color-bg);
		padding: 0.125rem 0.5rem;
		border-radius: 9999px;
	}

	.column-tasks {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		min-height: 200px;
	}

	.task-card {
		background: var(--color-bg);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		padding: 0.75rem;
		cursor: grab;
		transition: box-shadow 0.15s;
	}

	.task-card:hover {
		box-shadow: var(--shadow-md);
	}

	.task-card:active {
		cursor: grabbing;
	}

	.task-title {
		font-weight: 500;
		font-size: 0.875rem;
		margin-bottom: 0.25rem;
	}

	.task-description {
		font-size: 0.75rem;
		color: var(--color-text-muted);
		margin-bottom: 0.5rem;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.task-meta {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.75rem;
	}

	.priority-badge {
		color: white;
		padding: 0.125rem 0.375rem;
		border-radius: var(--radius-sm);
		text-transform: capitalize;
	}

	.due-date {
		color: var(--color-text-muted);
	}
</style>
