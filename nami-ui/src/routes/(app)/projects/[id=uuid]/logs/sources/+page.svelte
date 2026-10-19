<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import { formatQuota, PLATFORM_LABELS, PLATFORMS } from '#lib/logs';
	import { formatUtc } from '#lib/uptime';
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
	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
</script>

<svelte:head>
	<title>Sources · Logs · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	{#if form?.created}
		<div class="space-y-3 rounded-xl border border-teal-500/30 bg-teal-500/10 p-6" role="status">
			<p class="text-sm text-teal-200">
				Source <span class="font-medium wrap-anywhere">{form.created.name}</span> created. Send logs with this token; project
				members can reveal it again on the source page.
			</p>
			<div role="group" aria-label="Source token" class="flex items-center gap-2">
				<code
					class="block flex-1 rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 font-mono text-xs break-all text-neutral-200 select-all"
					>{form.created.token}</code
				>
				<CopyButton value={form.created.token} />
			</div>
			<a href="{base}/{form.created.id}" class="text-sm text-teal-300 hover:text-teal-200">Setup instructions →</a>
		</div>
	{/if}

	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<div class="border-b border-[#262626] px-6 py-4">
			<h2 class="text-base font-semibold text-neutral-100">Log sources</h2>
			<p class="text-xs text-neutral-500">
				Each source has its own ingest token and retention. Send logs over HTTP (JSON, NDJSON, MessagePack) or syslog.
			</p>
		</div>
		{#if data.sources.length === 0}
			<p class="px-6 py-10 text-center text-sm text-neutral-500">No sources yet. Create one below.</p>
		{:else}
			<div class="overflow-x-auto">
				<table class="w-full text-left text-sm">
					<thead class="text-xs text-neutral-500">
						<tr class="border-b border-[#262626]">
							<th class="px-6 py-2 font-medium">Name</th>
							<th class="px-6 py-2 font-medium">Platform</th>
							<th class="px-6 py-2 font-medium">Retention</th>
							<th class="px-6 py-2 font-medium">Daily quota</th>
							<th class="px-6 py-2 font-medium">Created</th>
						</tr>
					</thead>
					<tbody>
						{#each data.sources as source (source.id)}
							<tr class="border-b border-[#262626] last:border-0">
								<td class="px-6 py-3">
									<a href="{base}/{source.id}" class="font-medium wrap-anywhere text-neutral-100 hover:text-teal-300"
										>{source.name}</a
									>
								</td>
								<td class="px-6 py-3 text-neutral-400">{PLATFORM_LABELS[source.platform]}</td>
								<td class="px-6 py-3 text-neutral-400">{source.retention_days} days</td>
								<td class="px-6 py-3 text-neutral-400">{formatQuota(source.daily_byte_quota)}</td>
								<td class="px-6 py-3 text-xs text-neutral-500">{formatUtc(source.created_at)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-base font-semibold text-neutral-100">New source</h2>
		{#if form?.message}
			<div class="mt-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400" role="alert">
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
			<button
				type="submit"
				disabled={busy}
				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
			>
				Create
			</button>
		</form>
	</div>
</div>
