<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import MaintenanceWindowForm from '#lib/components/MaintenanceWindowForm.svelte';
	import { describeSchedule, describeScope } from '#lib/maintenance';
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
	const button =
		'rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400 disabled:cursor-not-allowed disabled:opacity-50';
</script>

<svelte:head>
	<title>Maintenance · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<div class="border-b border-[#262626] px-6 py-4">
			<h2 class="text-base font-semibold text-neutral-100">Maintenance windows</h2>
			<p class="text-xs text-neutral-500">
				While a window is active its monitors and heartbeats are still checked, but they open no incidents and send
				no alerts.
			</p>
		</div>
		{#if form?.action === 'delete' && 'message' in form && form.message}
			<div
				role={form.success ? 'status' : 'alert'}
				class="mx-6 mt-4 rounded-md border px-3 py-2 text-sm {form.success
					? 'border-teal-500/30 bg-teal-500/10 text-teal-300'
					: 'border-red-500/30 bg-red-500/10 text-red-400'}"
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
				class="mx-6 mt-4 flex flex-wrap items-center gap-3 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-300"
			>
				<input type="hidden" name="window_id" value={pending.id} />
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">Delete maintenance window "{pending.title}"?</span>
				<button type="submit" disabled={busy} class={button}>Yes, delete</button>
				<a href="/projects/{data.project.id}/maintenance" class="text-neutral-400 hover:text-neutral-200">Cancel</a>
			</form>
		{/if}
		{#if data.windows.length === 0}
			<p class="px-6 py-10 text-center text-sm text-neutral-500">No maintenance windows yet — schedule one below.</p>
		{:else}
			<ul>
				{#each data.windows as window (window.id)}
					<li class="flex flex-wrap items-center justify-between gap-3 border-b border-[#262626] px-6 py-4 last:border-0">
						<div class="min-w-0 space-y-1">
							<div class="flex flex-wrap items-center gap-2">
								<span class="font-medium wrap-anywhere text-neutral-100">{window.title}</span>
								{#if window.active}
									<span class="rounded-full border border-sky-500/30 bg-sky-500/10 px-2 py-0.5 text-xs text-sky-300">
										Active now
									</span>
								{/if}
							</div>
							<p class="text-xs text-neutral-400">{describeSchedule(window)}</p>
							<p class="text-xs wrap-anywhere text-neutral-500">{describeScope(window.monitor_id, data.monitors)}</p>
						</div>
						<div class="flex items-center gap-2">
							<a
								href="/projects/{data.project.id}/maintenance/{window.id}"
								class="rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100"
								>Edit</a
							>
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

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-base font-semibold text-neutral-100">New maintenance window</h2>
		{#if form?.action === 'create' && form.success}
			<div role="status" class="mt-4 rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-2 text-sm text-teal-300">
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
