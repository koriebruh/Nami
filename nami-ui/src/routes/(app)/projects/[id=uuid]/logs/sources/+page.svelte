<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import { formatQuota, PLATFORM_LABELS, PLATFORMS } from '#lib/logs/logs';
	import { formatUtc } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	// Disabled from a submit until its result is applied: no double create.
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

	const base = $derived(`/projects/${data.project.id}/logs/sources`);
	const input = 'input';
	const label = 'label';
</script>

<svelte:head>
	<title>Sources · Logs · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	{#if form?.created}
		<div class="space-y-3 rounded-card border border-success-ink/25 bg-success-soft p-5 sm:p-6" role="status">
			<p class="text-sm text-success-ink">
				Source <span class="font-medium wrap-anywhere">{form.created.name}</span> created. Send logs with this token; project
				members can reveal it again on the source page.
			</p>
			<div role="group" aria-label="Source token" class="flex items-center gap-2">
				<code
					class="block flex-1 rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all text-ink select-all"
					>{form.created.token}</code
				>
				<CopyButton value={form.created.token} />
			</div>
			<a href="{base}/{form.created.id}" class="link text-sm">Setup instructions →</a>
		</div>
	{/if}

	<div class="card">
		<div class="border-b border-border px-5 py-4 sm:px-6">
			<h2 class="text-lg font-light text-ink">Log sources</h2>
			<p class="text-xs text-muted">
				Each source has its own ingest token and retention. Send logs over HTTP (JSON, NDJSON, MessagePack) or syslog.
			</p>
		</div>
		{#if data.sources.length === 0}
			<EmptyState icon="source" message="No sources yet. Create one below." />
		{:else}
			<div class="overflow-x-auto">
				<table class="table table-hover">
					<thead>
						<tr>
							<th>Name</th>
							<th>Platform</th>
							<th>Retention</th>
							<th>Daily quota</th>
							<th>Created</th>
						</tr>
					</thead>
					<tbody>
						{#each data.sources as source (source.id)}
							<tr>
								<td>
									<a href="{base}/{source.id}" class="font-medium wrap-anywhere text-ink hover:text-primary"
										>{source.name}</a
									>
								</td>
								<td>{PLATFORM_LABELS[source.platform]}</td>
								<td>{source.retention_days} days</td>
								<td>{formatQuota(source.daily_byte_quota)}</td>
								<td class="text-xs text-muted">{formatUtc(source.created_at)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</div>

	<div class="card p-5 sm:p-6">
		<h2 class="text-lg font-light text-ink">New source</h2>
		{#if form?.message}
			<div class="alert alert-danger mt-4" role="alert">
				{form.message}
			</div>
		{/if}
		<form
			method="POST"
			action="?/create"
			use:enhance={submit}
			class="mt-4 grid gap-4 sm:grid-cols-2 lg:grid-cols-[1fr_10rem_9rem_10rem_auto] lg:items-end"
		>
			<div>
				<label for="source-name" class={label}>Name</label>
				<input id="source-name" name="name" type="text" required maxlength="200" placeholder="Production API" class={input} />
			</div>
			<div>
				<label for="source-platform" class={label}>Platform</label>
				<select id="source-platform" name="platform" class={input}>
					{#each PLATFORMS as platform (platform)}
						<option value={platform}>{PLATFORM_LABELS[platform]}</option>
					{/each}
				</select>
			</div>
			<div>
				<label for="source-retention" class={label}>Retention (days)</label>
				<input id="source-retention" name="retention_days" type="number" required min="1" max="365" value="30" class={input} />
			</div>
			<div>
				<label for="source-quota" class={label}>Daily quota (MiB)</label>
				<input id="source-quota" name="daily_quota_mib" type="number" min="1" step="any" placeholder="Unlimited" class={input} />
			</div>
			<button type="submit" disabled={busy} class="btn btn-primary">Create</button>
		</form>
	</div>
</div>
