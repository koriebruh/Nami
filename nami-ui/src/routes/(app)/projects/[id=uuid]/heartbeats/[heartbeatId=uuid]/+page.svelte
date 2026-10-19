<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import { refreshAll } from '$app/navigation';
	import AvailabilityCard from '#lib/components/AvailabilityCard.svelte';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import { exitCodeCommand, failUrl, formatSeconds, formatUtc, pingCommand, pingUrl, timeAgo } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});

	const base = $derived(`/projects/${data.project.id}/heartbeats/${data.heartbeat.id}`);
	// The engine sends no token to a viewer: no ping URL and no command then.
	const url = $derived(data.heartbeat.token ? pingUrl(data.engineUrl, data.heartbeat.token) : null);
	const failEndpoint = $derived(url ? failUrl(url) : null);
	const exitCommand = $derived(url ? exitCodeCommand(url) : null);
	const cron = $derived(url ? `0 3 * * * /usr/local/bin/backup.sh && ${pingCommand(url)}` : null);

	// The settings inputs read these three instead of `data`. Every 30 s refresh
	// hands over a new `data` object, but a derived tells its readers only when
	// its value changed, so a refresh leaves a half-typed edit alone.
	const savedName = $derived(data.heartbeat.name);
	const savedPeriod = $derived(data.heartbeat.period_seconds);
	const savedGrace = $derived(data.heartbeat.grace_seconds);
	const savedGroup = $derived(data.heartbeat.group_id ?? '');

	// The buttons stay disabled from a submit until its result is applied, so a
	// double click cannot send an action twice. They are in the `submitButton`
	// snippet below on purpose: see there.
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
	// reset: false keeps the settings inputs showing what was just saved.
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

	// Delete and rotate ask with confirm() first. The dialog is the
	// confirmation, so the form sends `confirm=yes`; without JavaScript the
	// action asks on the page instead (see the panels below).
	const confirmDelete: SubmitFunction = (submission) => {
		if (!confirm(`Delete heartbeat "${data.heartbeat.name}"? Its incidents are deleted too.`)) {
			submission.cancel();
			return;
		}
		submission.formData.set('confirm', 'yes');
		return submit(submission);
	};
	const confirmRotate: SubmitFunction = (submission) => {
		if (
			!confirm(
				`Rotate the ping token of "${data.heartbeat.name}"? The old ping URL stops working immediately, so update every cron job that uses it.`
			)
		) {
			submission.cancel();
			return;
		}
		submission.formData.set('confirm', 'yes');
		return submit(submission);
	};

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
	<title>{data.heartbeat.name} · Heartbeats</title>
</svelte:head>

<!-- Svelte sets a `value={...}` input from the effect that holds it, and the
     first re-run of that effect after hydration overwrites what the user typed
     with the saved value. So the settings inputs sit in their own effect (the
     `{#key}` block below) and read the `saved...` deriveds, which only change
     when the saved value does. The buttons are rendered from this snippet for
     the same reason: it has its own effect, so `busy` changing re-runs nothing
     but the buttons. -->
{#snippet submitButton(text: string, classes: string)}
	<button type="submit" disabled={busy} class={classes}>{text}</button>
{/snippet}

<div class="space-y-6">
	<div class="card p-5 sm:p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div class="space-y-2">
				<div class="flex flex-wrap items-center gap-3">
					<StatusBadge status={data.heartbeat.current_status} paused={data.heartbeat.paused} />
					{#if data.heartbeat.in_maintenance}<MaintenanceBadge />{/if}
				</div>
				<h2 class="text-lg font-light wrap-anywhere text-ink">{data.heartbeat.name}</h2>
				<p class="text-sm text-muted">
					Last ping:
					{#if data.heartbeat.last_ping_at}
						{formatUtc(data.heartbeat.last_ping_at)} ({timeAgo(data.heartbeat.last_ping_at, now)})
					{:else}
						never
					{/if}
				</p>
				<p class="text-sm text-muted">
					Expected every {formatSeconds(data.heartbeat.period_seconds)}, with {formatSeconds(data.heartbeat.grace_seconds)} grace.
				</p>
			</div>
			<div class="flex flex-wrap gap-2">
				{#if data.heartbeat.paused}
					<form method="POST" action="?/resume" use:enhance={submit}>{@render submitButton('Resume', button)}</form>
				{:else}
					<form method="POST" action="?/pause" use:enhance={submit}>{@render submitButton('Pause', button)}</form>
				{/if}
				<form method="POST" action="?/delete" use:enhance={confirmDelete}>
					{@render submitButton('Delete', dangerButton)}
				</form>
			</div>
		</div>
		{#if form?.message && (form.action === 'pause' || form.action === 'resume' || form.action === 'delete')}
			<div role="alert" class="mt-4 {alertBox}">{form.message}</div>
		{/if}
		<!-- The second step of a delete without JavaScript (see the delete action). -->
		{#if form?.confirmDelete}
			<form method="POST" action="?/delete" use:enhance={submit} class="mt-4 {confirmBox}">
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">Delete heartbeat "{data.heartbeat.name}"? Its incidents are deleted too.</span>
				{@render submitButton('Yes, delete', `btn btn-danger-solid btn-sm`)}
				<a href={base} class="text-ink-2 hover:text-ink">Cancel</a>
			</form>
		{/if}
	</div>

	<div class="card space-y-3 p-5 sm:p-6">
		<h3 class="card-title">Ping URL</h3>
		{#if url && cron}
			<p class="text-sm text-muted">
				Request it (GET or POST, no login needed) at the end of every successful run.
			</p>
			<div role="group" aria-label="Ping URL" class="flex items-center gap-2">
				<code class="block flex-1 rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all text-ink-2 select-all">{url}</code>
				<CopyButton value={url} />
			</div>
			<p class="pt-2 text-sm text-muted">Example crontab entry:</p>
			<div role="group" aria-label="Example crontab entry" class="flex items-center gap-2">
				<code class="block flex-1 rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all text-ink-2 select-all">{cron}</code>
				<CopyButton value={cron} />
			</div>

			{#if failEndpoint && exitCommand}
				<div class="space-y-3 border-t border-border pt-4">
					<h4 class="text-sm font-medium text-ink">Report a failure</h4>
					<p class="text-sm text-muted">
						Request the fail URL when a run goes wrong to open an incident right away, without waiting for the grace period.
					</p>
					<div role="group" aria-label="Fail URL" class="flex items-center gap-2">
						<code class="block flex-1 rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all text-ink-2 select-all">{failEndpoint}</code>
						<CopyButton value={failEndpoint} />
					</div>
					<p class="pt-2 text-sm text-muted">
						Or report the job's exit code (0 is a success, anything else a failure) with the end of its output (the last 10 KiB) as the request body:
					</p>
					<div role="group" aria-label="Exit code command" class="flex items-start gap-2">
						<pre class="block flex-1 rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all whitespace-pre-wrap text-ink-2 select-all">{exitCommand}</pre>
						<CopyButton value={exitCommand} />
					</div>
				</div>
			{/if}

			<div class="space-y-3 border-t border-border pt-4">
				<div class="flex flex-wrap items-center gap-3">
					<form method="POST" action="?/rotate" use:enhance={confirmRotate}>
						{@render submitButton('Rotate token', button)}
					</form>
					<p class="text-xs text-muted">
						Gives the heartbeat a new ping URL. The old URL stops working immediately.
					</p>
				</div>
				{#if form?.rotated}
					<div role="status" class="alert alert-success">
						Token rotated. The old ping URL no longer works: use the new one above.
					</div>
				{/if}
				{#if form?.message && form.action === 'rotate'}
					<div role="alert" class={alertBox}>{form.message}</div>
				{/if}
				<!-- The second step of a rotate without JavaScript (see the rotate action). -->
				{#if form?.confirmRotate}
					<form method="POST" action="?/rotate" use:enhance={submit} class={confirmBox}>
						<input type="hidden" name="confirm" value="yes" />
						<span class="wrap-anywhere">
							Rotate the ping token of "{data.heartbeat.name}"? The old ping URL stops working immediately, so update
							every cron job that uses it.
						</span>
						{@render submitButton('Yes, rotate', `btn btn-danger-solid btn-sm`)}
						<a href={base} class="text-ink-2 hover:text-ink">Cancel</a>
					</form>
				{/if}
			</div>
		{:else}
			<p class="text-sm text-muted">Ping URL is visible to project members.</p>
		{/if}
	</div>

	<div class="card" aria-labelledby="pings-title">
		<div class="border-b border-border px-5 py-4 sm:px-6">
			<h3 id="pings-title" class="card-title">Recent pings</h3>
		</div>
		{#if data.pings.length === 0}
			<EmptyState icon="heartbeat" message="No pings received yet." />
		{:else}
			<ul class="divide-y divide-border">
				{#each data.pings as ping, i (i)}
					<li class="space-y-2 px-5 py-3 sm:px-6">
						<div class="flex flex-wrap items-center gap-3 text-sm">
							<span class="badge {ping.kind === 'fail' ? 'badge-danger' : 'badge-success'}">
								{ping.kind === 'fail' ? 'Fail' : 'Success'}
							</span>
							<span class="text-ink-2">{formatUtc(ping.received_at)}</span>
							{#if ping.exit_code !== null}<span class="text-xs text-muted">exit code {ping.exit_code}</span>{/if}
						</div>
						{#if ping.output}
							<!-- Output is text from the job: interpolated, never markup. -->
							<details>
								<summary class="cursor-pointer text-xs text-primary">Output</summary>
								<pre class="mt-2 max-h-64 overflow-auto rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all whitespace-pre-wrap text-ink-2">{ping.output}</pre>
							</details>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</div>

	<AvailabilityCard availability={data.availability} />

	<div class="card p-5 sm:p-6">
		<h3 class="card-title">Settings</h3>
		{#if form?.saved}
			<div role="status" class="alert alert-success mt-4">Saved.</div>
		{/if}
		{#if form?.message && form.action === 'update'}
			<div role="alert" class="mt-4 {alertBox}">{form.message}</div>
		{/if}
		<!-- Keyed, so that opening another heartbeat starts from its own values. -->
		{#key data.heartbeat.id}
			<form
				method="POST"
				action="?/update"
				use:enhance={submitKeepingValues}
				class="mt-4 grid gap-4 sm:grid-cols-[1fr_10rem_10rem_auto] sm:items-end"
			>
				<div>
					<label for="hb-name" class={label}>Name</label>
					<input id="hb-name" name="name" type="text" required maxlength="200" value={savedName} class={input} />
				</div>
				<div>
					<label for="hb-period" class={label}>Period (seconds)</label>
					<input
						id="hb-period"
						name="period_seconds"
						type="number"
						required
						min="30"
						max="2678400"
						aria-describedby="hb-hint"
						value={savedPeriod}
						class={input}
					/>
				</div>
				<div>
					<label for="hb-grace" class={label}>Grace (seconds)</label>
					<input
						id="hb-grace"
						name="grace_seconds"
						type="number"
						required
						min="0"
						max="2678400"
						aria-describedby="hb-hint"
						value={savedGrace}
						class={input}
					/>
				</div>
				{#if data.groups.length > 0}
					<div class="sm:col-span-3 sm:max-w-sm">
						<label for="hb-group" class={label}>Group</label>
						<select id="hb-group" name="group_id" class="select">
							<option value="">No group</option>
							{#each data.groups as group (group.id)}
								<option value={group.id} selected={group.id === savedGroup}>{group.name}</option>
							{/each}
						</select>
					</div>
				{:else if savedGroup}
					<!-- Groups did not load: keep the heartbeat's group instead of ungrouping it. -->
					<input type="hidden" name="group_id" value={savedGroup} />
				{/if}
				{@render submitButton('Save', primaryButton)}
				<p id="hb-hint" class="text-xs text-muted sm:col-span-4">
					Period: at least 30, grace: 0 or more, both at most 2678400 (31 days).
				</p>
			</form>
		{/key}
	</div>
</div>
