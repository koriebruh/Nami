<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import {
		curlSnippet,
		dockerSnippet,
		formatBytes,
		formatQuota,
		ingestUrl,
		PLATFORM_LABELS,
		PLATFORMS,
		quotaInMib,
		rsyslogSnippet,
		TOKEN_PLACEHOLDER,
		vectorSnippet
	} from '#lib/logs';
	import { formatUtc } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	type SnippetKind = 'curl' | 'vector' | 'syslog' | 'docker';
	const SNIPPET_LABELS: Record<SnippetKind, string> = {
		curl: 'curl',
		vector: 'Vector',
		syslog: 'Syslog (rsyslog)',
		docker: 'Docker'
	};
	let snippetKind = $state<SnippetKind>('curl');

	const base = $derived(`/projects/${data.project.id}/logs/sources/${data.source.id}`);
	const token = $derived(form?.token ?? TOKEN_PLACEHOLDER);
	const url = $derived(ingestUrl(data.engineUrl));
	const snippet = $derived.by(() => {
		switch (snippetKind) {
			case 'curl':
				return curlSnippet(url, token);
			case 'vector':
				return vectorSnippet(url, token);
			case 'syslog':
				return rsyslogSnippet(data.syslog.host, data.syslog.port, token);
			case 'docker':
				return dockerSnippet(url, token);
		}
	});

	// The settings inputs read these instead of `data`, so that a reload of the
	// page data leaves a half-typed edit alone (a derived only notifies when its
	// value changed).
	const savedName = $derived(data.source.name);
	const savedPlatform = $derived(data.source.platform);
	const savedRetention = $derived(data.source.retention_days);
	const savedQuota = $derived(quotaInMib(data.source.daily_byte_quota));

	// Buttons stay disabled from a submit until its result is applied. They are
	// rendered from a snippet so that `busy` re-runs nothing but the buttons.
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
	const submitKeepingValues: SubmitFunction = () => {
		busy = true;
		return async ({ update }) => {
			try {
				await update({ reset: false });
			} finally {
				busy = false;
			}
		};
	};
	// Rotate and delete ask with confirm() first; the dialog is the
	// confirmation, so the form sends `confirm=yes`. Without JavaScript the
	// action asks on the page instead (see the panels below).
	const confirmed =
		(question: string): SubmitFunction =>
		(submission) => {
			if (!confirm(question)) {
				submission.cancel();
				return;
			}
			submission.formData.set('confirm', 'yes');
			return submit(submission);
		};
	const rotateQuestion = $derived(
		`Rotate the token of "${data.source.name}"? The current token stops working immediately, so update every sender that uses it.`
	);
	const deleteQuestion = $derived(
		`Delete source "${data.source.name}"? Ingestion stops now; stored logs age out with their retention.`
	);

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
	const disabled = 'disabled:cursor-not-allowed disabled:opacity-50';
	const button = `rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100 ${disabled}`;
	const dangerButton = `rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400 ${disabled}`;
	const primaryButton = `rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none ${disabled}`;
	const alertBox = 'rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400';
	const confirmBox =
		'flex flex-wrap items-center gap-3 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-300';
</script>

<svelte:head>
	<title>{data.source.name} · Sources · Logs</title>
</svelte:head>

{#snippet submitButton(text: string, classes: string)}
	<button type="submit" disabled={busy} class={classes}>{text}</button>
{/snippet}

<div class="space-y-6">
	<a href="/projects/{data.project.id}/logs/sources" class="text-sm text-neutral-500 hover:text-neutral-300">← Sources</a>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div class="space-y-1">
				<h2 class="text-base font-semibold wrap-anywhere text-neutral-100">{data.source.name}</h2>
				<p class="text-sm text-neutral-500">
					{PLATFORM_LABELS[data.source.platform]} · {data.source.retention_days} days retention · {formatQuota(
						data.source.daily_byte_quota
					)}
				</p>
				<p class="text-xs text-neutral-600">Created {formatUtc(data.source.created_at)}</p>
			</div>
			<form method="POST" action="?/delete" use:enhance={confirmed(deleteQuestion)}>
				{@render submitButton('Delete', dangerButton)}
			</form>
		</div>
		{#if form?.message && form.action === 'delete'}
			<div role="alert" class="mt-4 {alertBox}">{form.message}</div>
		{/if}
		<!-- The second step of a delete without JavaScript (see the delete action). -->
		{#if form?.confirmDelete}
			<form method="POST" action="?/delete" use:enhance={submit} class="mt-4 {confirmBox}">
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">{deleteQuestion}</span>
				{@render submitButton('Yes, delete', `${dangerButton} border-red-500/40 text-red-300`)}
				<a href={base} class="text-neutral-400 hover:text-neutral-200">Cancel</a>
			</form>
		{/if}
	</div>

	<div class="space-y-3 rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Source token</h3>
		{#if form?.token}
			<div role="group" aria-label="Source token" class="flex items-center gap-2">
				<code
					class="block flex-1 rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 font-mono text-xs break-all text-neutral-200 select-all"
					>{form.token}</code
				>
				<CopyButton value={form.token} />
			</div>
			<p class="text-xs text-neutral-500">Shown until you leave this page. Treat it like a password.</p>
		{:else}
			<p class="text-sm text-neutral-500">
				The token is visible to project members. Revealing it also fills it into the snippets below.
			</p>
			<form method="POST" action="?/reveal" use:enhance={submit}>
				{@render submitButton('Reveal token', button)}
			</form>
		{/if}
		{#if form?.message && (form.action === 'reveal' || form.action === 'rotate')}
			<div role="alert" class={alertBox}>{form.message}</div>
		{/if}
		{#if form?.rotated}
			<div role="status" class="rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-2 text-sm text-teal-300">
				Token rotated. Update your senders: the old token no longer works.
			</div>
		{/if}
		<div class="flex flex-wrap items-center gap-3 border-t border-[#262626] pt-3">
			<form method="POST" action="?/rotate" use:enhance={confirmed(rotateQuestion)}>
				{@render submitButton('Rotate token', button)}
			</form>
			<p class="text-xs text-neutral-500">Gives the source a new token. The current one stops working immediately.</p>
		</div>
		<!-- The second step of a rotate without JavaScript (see the rotate action). -->
		{#if form?.confirmRotate}
			<form method="POST" action="?/rotate" use:enhance={submit} class={confirmBox}>
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">{rotateQuestion}</span>
				{@render submitButton('Yes, rotate', `${dangerButton} border-red-500/40 text-red-300`)}
				<a href={base} class="text-neutral-400 hover:text-neutral-200">Cancel</a>
			</form>
		{/if}
	</div>

	<div class="space-y-3 rounded-xl border border-[#262626] bg-[#141414] p-6">
		<div class="flex flex-wrap items-center justify-between gap-2">
			<h3 class="text-sm font-semibold text-neutral-200">Send logs</h3>
			<div class="flex gap-1" role="tablist" aria-label="Setup snippets">
				{#each Object.entries(SNIPPET_LABELS) as [kind, title] (kind)}
					<button
						type="button"
						role="tab"
						aria-selected={snippetKind === kind}
						onclick={() => (snippetKind = kind as SnippetKind)}
						class="rounded-md px-2.5 py-1 text-xs transition-colors {snippetKind === kind
							? 'bg-[#1f1f1f] text-neutral-100'
							: 'text-neutral-500 hover:text-neutral-300'}">{title}</button
					>
				{/each}
			</div>
		</div>
		<div class="flex items-start gap-2">
			<pre
				class="min-w-0 flex-1 overflow-x-auto rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 font-mono text-xs text-neutral-300">{snippet}</pre>
			<CopyButton value={snippet} />
		</div>
		<p class="text-xs text-neutral-500">
			HTTP accepts a JSON object, a JSON array, NDJSON or MessagePack (gzip optional) up to 10 MiB and 10,000 events per
			request, 50 requests per second per source. Syslog needs the engine's <code>SYSLOG_ENABLED=true</code>.
		</p>
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Today (UTC)</h3>
		{#if data.today}
			<dl class="mt-3 grid grid-cols-2 gap-4 text-sm sm:grid-cols-4">
				<div>
					<dt class="text-xs text-neutral-500">Events</dt>
					<dd class="text-neutral-100 tabular-nums">{data.today.events.toLocaleString('en-US')}</dd>
				</div>
				<div>
					<dt class="text-xs text-neutral-500">Volume</dt>
					<dd class="text-neutral-100 tabular-nums">{formatBytes(data.today.bytes)}</dd>
				</div>
				<div>
					<dt class="text-xs text-neutral-500">Rate limited</dt>
					<dd class="text-neutral-100 tabular-nums">{data.today.requests_rate_limited.toLocaleString('en-US')}</dd>
				</div>
				<div>
					<dt class="text-xs text-neutral-500">Over quota</dt>
					<dd class="text-neutral-100 tabular-nums">{data.today.requests_over_quota.toLocaleString('en-US')}</dd>
				</div>
			</dl>
			<p class="mt-2 text-xs text-neutral-600">Counts reach this page within about 10 seconds.</p>
		{:else}
			<p class="mt-2 text-sm text-neutral-500">No usage data.</p>
		{/if}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Settings</h3>
		{#if form?.saved}
			<div role="status" class="mt-4 rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-2 text-sm text-teal-300">Saved.</div>
		{/if}
		{#if form?.message && form.action === 'update'}
			<div role="alert" class="mt-4 {alertBox}">{form.message}</div>
		{/if}
		<!-- Keyed, so that opening another source starts from its own values. -->
		{#key data.source.id}
			<form method="POST" action="?/update" use:enhance={submitKeepingValues} class="mt-4 grid gap-4 sm:grid-cols-2">
				<div>
					<label for="source-name" class={label}>Name</label>
					<input id="source-name" name="name" type="text" required maxlength="200" value={savedName} class={input} />
				</div>
				<div>
					<label for="source-platform" class={label}>Platform</label>
					<select id="source-platform" name="platform" value={savedPlatform} class={input}>
						{#each PLATFORMS as platform (platform)}
							<option value={platform}>{PLATFORM_LABELS[platform]}</option>
						{/each}
					</select>
				</div>
				<div>
					<label for="source-retention" class={label}>Retention (days)</label>
					<input
						id="source-retention"
						name="retention_days"
						type="number"
						required
						min="1"
						max="365"
						value={savedRetention}
						class={input}
					/>
					<p class="mt-1 text-xs text-neutral-500">Changes affect new rows only; stored rows keep their retention.</p>
				</div>
				<div>
					<label for="source-quota" class={label}>Daily quota (MiB)</label>
					<input
						id="source-quota"
						name="daily_quota_mib"
						type="number"
						min="1"
						step="any"
						placeholder="Unlimited"
						value={savedQuota}
						class={input}
					/>
					<p class="mt-1 text-xs text-neutral-500">
						Over the quota, requests get 429 <code>quota_exceeded</code> until midnight UTC; one request plus about 10 s of
						traffic per engine instance can go over.
					</p>
				</div>
				<div class="sm:col-span-2">
					{@render submitButton('Save', primaryButton)}
				</div>
			</form>
		{/key}
	</div>
</div>
