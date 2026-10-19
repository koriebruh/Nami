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
	} from '#lib/logs/logs';
	import { formatUtc } from '#lib/uptime/uptime';
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

	const input = 'input';
	const label = 'label';
	const button = `btn btn-secondary btn-sm`;
	const dangerButton = `btn btn-danger btn-sm`;
	const primaryButton = `btn btn-primary`;
	const alertBox = 'alert alert-danger';
	const confirmBox =
		'alert alert-danger flex flex-wrap items-center gap-3';
</script>

<svelte:head>
	<title>{data.source.name} · Sources · Logs</title>
</svelte:head>

{#snippet submitButton(text: string, classes: string)}
	<button type="submit" disabled={busy} class={classes}>{text}</button>
{/snippet}

<div class="space-y-6">
	<a href="/projects/{data.project.id}/logs/sources" class="text-sm text-muted hover:text-primary">← Sources</a>

	<div class="card p-5 sm:p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div class="space-y-1">
				<h2 class="text-lg font-light wrap-anywhere text-ink">{data.source.name}</h2>
				<p class="text-sm text-muted">
					{PLATFORM_LABELS[data.source.platform]} · {data.source.retention_days} days retention · {formatQuota(
						data.source.daily_byte_quota
					)}
				</p>
				<p class="text-xs text-muted">Created {formatUtc(data.source.created_at)}</p>
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
				{@render submitButton('Yes, delete', `btn btn-danger-solid btn-sm`)}
				<a href={base} class="text-ink-2 hover:text-ink">Cancel</a>
			</form>
		{/if}
	</div>

	<div class="card space-y-3 p-5 sm:p-6">
		<h3 class="card-title">Source token</h3>
		{#if form?.token}
			<div role="group" aria-label="Source token" class="flex items-center gap-2">
				<code
					class="block flex-1 rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all text-ink select-all"
					>{form.token}</code
				>
				<CopyButton value={form.token} />
			</div>
			<p class="text-xs text-muted">Shown until you leave this page. Treat it like a password.</p>
		{:else}
			<p class="text-sm text-muted">
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
			<div role="status" class="alert alert-success">
				Token rotated. Update your senders: the old token no longer works.
			</div>
		{/if}
		<div class="flex flex-wrap items-center gap-3 border-t border-border pt-3">
			<form method="POST" action="?/rotate" use:enhance={confirmed(rotateQuestion)}>
				{@render submitButton('Rotate token', button)}
			</form>
			<p class="text-xs text-muted">Gives the source a new token. The current one stops working immediately.</p>
		</div>
		<!-- The second step of a rotate without JavaScript (see the rotate action). -->
		{#if form?.confirmRotate}
			<form method="POST" action="?/rotate" use:enhance={submit} class={confirmBox}>
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">{rotateQuestion}</span>
				{@render submitButton('Yes, rotate', `btn btn-danger-solid btn-sm`)}
				<a href={base} class="text-ink-2 hover:text-ink">Cancel</a>
			</form>
		{/if}
	</div>

	<div class="card space-y-3 p-5 sm:p-6">
		<div class="flex flex-wrap items-center justify-between gap-2">
			<h3 class="card-title">Send logs</h3>
			<div class="flex gap-1" role="tablist" aria-label="Setup snippets">
				{#each Object.entries(SNIPPET_LABELS) as [kind, title] (kind)}
					<button
						type="button"
						role="tab"
						aria-selected={snippetKind === kind}
						onclick={() => (snippetKind = kind as SnippetKind)}
						class="rounded-control border px-2.5 py-1 text-xs transition-colors {snippetKind === kind
							? 'border-primary text-primary'
							: 'border-transparent text-ink-2 hover:text-primary'}">{title}</button
					>
				{/each}
			</div>
		</div>
		<div class="flex items-start gap-2">
			<pre
				class="min-w-0 flex-1 overflow-x-auto rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs text-ink-2">{snippet}</pre>
			<CopyButton value={snippet} />
		</div>
		<p class="text-xs text-muted">
			HTTP accepts a JSON object, a JSON array, NDJSON or MessagePack (gzip optional) up to 10 MiB and 10,000 events per
			request, 50 requests per second per source. Syslog needs the engine's <code>SYSLOG_ENABLED=true</code>.
		</p>
	</div>

	<div class="card p-5 sm:p-6">
		<h3 class="card-title">Today (UTC)</h3>
		{#if data.today}
			<dl class="mt-3 grid grid-cols-2 gap-4 text-sm sm:grid-cols-4">
				<div>
					<dt class="text-xs text-muted">Events</dt>
					<dd class="mt-1 text-2xl font-normal tracking-tight text-ink tabular-nums">{data.today.events.toLocaleString('en-US')}</dd>
				</div>
				<div>
					<dt class="text-xs text-muted">Volume</dt>
					<dd class="mt-1 text-2xl font-normal tracking-tight text-ink tabular-nums">{formatBytes(data.today.bytes)}</dd>
				</div>
				<div>
					<dt class="text-xs text-muted">Rate limited</dt>
					<dd class="mt-1 text-2xl font-normal tracking-tight text-ink tabular-nums">{data.today.requests_rate_limited.toLocaleString('en-US')}</dd>
				</div>
				<div>
					<dt class="text-xs text-muted">Over quota</dt>
					<dd class="mt-1 text-2xl font-normal tracking-tight text-ink tabular-nums">{data.today.requests_over_quota.toLocaleString('en-US')}</dd>
				</div>
			</dl>
			<p class="mt-2 text-xs text-muted">Counts reach this page within about 10 seconds.</p>
		{:else}
			<p class="mt-2 text-sm text-muted">No usage data.</p>
		{/if}
	</div>

	<div class="card p-5 sm:p-6">
		<h3 class="card-title">Settings</h3>
		{#if form?.saved}
			<div role="status" class="alert alert-success mt-4">Saved.</div>
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
					<p class="hint">Changes affect new rows only; stored rows keep their retention.</p>
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
					<p class="hint">
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
