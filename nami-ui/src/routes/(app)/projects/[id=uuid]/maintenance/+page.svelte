<script lang="ts">
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import { enhance, type SubmitFunction } from '$app/forms';
	import MaintenanceWindowForm from '#lib/components/MaintenanceWindowForm.svelte';
	import { describeSchedule, describeScope } from '#lib/uptime/maintenance';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	// Buttons stay disabled from a submit until its result is applied.
	let busy = $state(false);
	const submit: SubmitFunction = () => {
		busy = true;
		return async ({ update }) => {
			try {
				await update();
			} finally {
				busy = false;
			}
		};
	};
	// Delete asks with confirm() first. The dialog is the confirmation, so the
	// form sends `confirm=yes`; without JavaScript the action asks on the page
	// instead (see below).
	const confirmDelete =
		(title: string): SubmitFunction =>
		(submission) => {
			if (!confirm(`Delete maintenance window "${title}"?`)) {
				submission.cancel();
				return;
			}
			submission.formData.set('confirm', 'yes');
			return submit(submission);
		};

	const pending = $derived(
		form && 'confirmDelete' in form ? data.windows.find((w) => w.id === form.confirmDelete) : undefined
	);
	const button = 'btn btn-danger btn-sm';
</script>

<svelte:head>
	<title>Maintenance · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<div class="card">
		<div class="border-b border-border px-5 py-4 sm:px-6">
			<h2 class="text-lg font-light text-ink">Maintenance windows</h2>
			<p class="text-xs text-muted">
				While a window is active its monitors and heartbeats are still checked, but they open no incidents and send
				no alerts.
			</p>
		</div>
		{#if form?.action === 'delete' && 'message' in form && form.message}
			<div
				role={form.success ? 'status' : 'alert'}
				class="alert mx-5 mt-4 sm:mx-6 {form.success ? 'alert-success' : 'alert-danger'}"
			>
				{form.message}
			</div>
		{/if}
		<!-- The second step of a delete without JavaScript (see the delete action). -->
		{#if pending}
			<form
				method="POST"
				action="?/delete"
				use:enhance={submit}
				class="alert alert-danger mx-5 mt-4 flex flex-wrap items-center gap-3 sm:mx-6"
			>
				<input type="hidden" name="window_id" value={pending.id} />
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">Delete maintenance window "{pending.title}"?</span>
				<button type="submit" disabled={busy} class="btn btn-danger-solid btn-sm">Yes, delete</button>
				<a href="/projects/{data.project.id}/maintenance" class="text-ink-2 hover:text-ink">Cancel</a>
			</form>
		{/if}
		{#if data.windows.length === 0}
			<EmptyState icon="maintenance" message="No maintenance windows yet — schedule one below." />
		{:else}
			<ul>
				{#each data.windows as window (window.id)}
					<li class="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-4 last:border-0 sm:px-6">
						<div class="min-w-0 space-y-1">
							<div class="flex flex-wrap items-center gap-2">
								<span class="font-medium wrap-anywhere text-ink">{window.title}</span>
								{#if window.active}
									<span class="badge badge-info">
										<span class="h-1.5 w-1.5 rounded-full bg-status-maintenance" aria-hidden="true"></span>
										Active now
									</span>
								{/if}
							</div>
							<p class="text-xs text-ink-2">{describeSchedule(window)}</p>
							<p class="text-xs wrap-anywhere text-muted">{describeScope(window.monitor_id, data.monitors)}</p>
						</div>
						<div class="flex items-center gap-2">
							<a href="/projects/{data.project.id}/maintenance/{window.id}" class="btn btn-secondary btn-sm">Edit</a>
							<form method="POST" action="?/delete" use:enhance={confirmDelete(window.title)}>
								<input type="hidden" name="window_id" value={window.id} />
								<button type="submit" disabled={busy} class={button}>Delete</button>
							</form>
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	</div>

	<div class="card p-5 sm:p-6">
		<h2 class="text-lg font-light text-ink">New maintenance window</h2>
		{#if form?.action === 'create' && form.success}
			<div role="status" class="alert alert-success mt-4">
				{form.message}
			</div>
		{/if}
		<div class="mt-4">
			<MaintenanceWindowForm
				action="?/create"
				monitors={data.monitors}
				submitLabel="Schedule window"
				message={form?.action === 'create' && !form.success ? form.message : null}
			/>
		</div>
	</div>
</div>
