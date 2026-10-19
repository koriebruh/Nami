<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import { refreshAll } from '$app/navigation';
	import AvailabilityCard from '#lib/components/AvailabilityCard.svelte';
	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
	import ResponseTimeChart from '#lib/components/ResponseTimeChart.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import Card from '#lib/components/ui/Card.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import StatCard from '#lib/components/ui/StatCard.svelte';
	import {
		CHECK_TYPE_LABELS,
		formatDuration,
		formatMs,
		formatUptime,
		formatUtc,
		monitorTarget,
		statusSince,
		timeAgo
	} from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	// Advanced options, listed only when they differ from the defaults.
	const advancedRows = $derived.by(() => {
		const m = data.monitor;
		const rows: { label: string; value: string }[] = [];
		if (m.ip_version !== 'any') rows.push({ label: 'IP version', value: m.ip_version === 'ipv4' ? 'IPv4 only' : 'IPv6 only' });
		if (m.remember_cookies) rows.push({ label: 'Cookies', value: 'remembered during a check' });
		if (m.proxy_url) {
			const auth = m.proxy_username ? `, user ${m.proxy_username}${m.has_proxy_password ? ', password set' : ''}` : m.has_proxy_password ? ', password set' : '';
			rows.push({ label: 'Proxy', value: `${m.proxy_url}${auth}` });
		}
		return rows;
	});

	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});

	const base = $derived(`/projects/${data.project.id}/monitors/${data.monitor.id}`);
	const since = $derived(statusSince(data.monitor, data.incidents));
	const headline = $derived(
		data.monitor.paused
			? { text: 'PAUSED', class: 'text-paused-ink' }
			: data.monitor.current_status === 'up'
				? { text: 'UP', class: 'text-success-ink' }
				: data.monitor.current_status === 'down'
					? { text: 'DOWN', class: 'text-danger-ink' }
					: { text: 'PENDING', class: 'text-warning-ink' }
	);
	// Sparklines come from the 24 h series the page already loads.
	const upSeries = $derived(data.stats?.series.map((b) => b.up_ratio) ?? []);
	const responseSeries = $derived(data.stats?.series.map((b) => b.avg_response_ms) ?? []);
	const tiles = $derived([
		{ label: 'Uptime, 24 hours', value: formatUptime(data.stats?.uptime_24h), spark: upSeries, tone: 'success' as const },
		{ label: 'Uptime, 7 days', value: formatUptime(data.stats?.uptime_7d), spark: undefined, tone: 'success' as const },
		{ label: 'Uptime, 30 days', value: formatUptime(data.stats?.uptime_30d), spark: undefined, tone: 'success' as const },
		{
			label: 'Avg response, 24 hours',
			value: formatMs(data.stats?.avg_response_ms_24h),
			spark: responseSeries,
			tone: 'primary' as const
		}
	]);

	// The buttons stay disabled from a submit until its result is applied, so a
	// double click cannot send an action twice.
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
	// form sends `confirm=yes`; without JavaScript the action asks on the page.
	const confirmDelete: SubmitFunction = (input) => {
		if (!confirm(`Delete monitor "${data.monitor.name}"? Its incidents are deleted too.`)) {
			input.cancel();
			return;
		}
		input.formData.set('confirm', 'yes');
		return submit(input);
	};

	const button = 'btn btn-secondary';
</script>

<svelte:head>
	<title>{data.monitor.name} · Monitors</title>
</svelte:head>

<div class="space-y-6">
	<section class="card p-5 sm:p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div class="min-w-0">
				<div class="flex flex-wrap items-center gap-3">
					<span class="relative flex h-3 w-3" aria-hidden="true">
						{#if !data.monitor.paused && data.monitor.current_status !== 'pending'}
							<span
								class="absolute inline-flex h-full w-full animate-ping rounded-full opacity-40 motion-reduce:hidden {data.monitor.current_status === 'up'
									? 'bg-status-up'
									: 'bg-status-down'}"
							></span>
						{/if}
						<span
							class="relative inline-flex h-3 w-3 rounded-full {data.monitor.paused
								? 'bg-status-paused'
								: data.monitor.current_status === 'up'
									? 'bg-status-up'
									: data.monitor.current_status === 'down'
										? 'bg-status-down'
										: 'bg-status-pending'}"
						></span>
					</span>
					<p class="text-4xl font-light tracking-tight {headline.class}">{headline.text}</p>
					{#if data.monitor.in_maintenance}<MaintenanceBadge />{/if}
				</div>
				<!-- The engine stores no pause time, and `since` is when the status
				     before the pause began, so a paused monitor shows no "since". -->
				<p class="mt-1 text-sm text-muted">
					{#if data.monitor.paused}
						Checks are paused.
					{:else}
						since {formatUtc(since)} ({timeAgo(since, now)})
					{/if}
				</p>
				<h2 class="mt-4 text-xl font-light wrap-anywhere text-ink">{data.monitor.name}</h2>
				<p class="font-mono text-xs break-all text-ink-2">{monitorTarget(data.monitor)}</p>
				<p class="mt-1 text-xs text-muted">
					{CHECK_TYPE_LABELS[data.monitor.check_type]} · checked every {data.monitor.interval_seconds}s · last check
					{timeAgo(data.monitor.last_checked_at, now)}
					{#if data.monitor.recovery_threshold > 1}
						· recovers after {data.monitor.recovery_threshold} successful checks
					{/if}
				</p>
				{#if advancedRows.length}
					<dl class="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
						{#each advancedRows as row (row.label)}
							<dt class="text-muted">{row.label}</dt>
							<dd class="font-mono wrap-anywhere text-ink-2">{row.value}</dd>
						{/each}
					</dl>
				{/if}
			</div>
			<div class="flex flex-wrap gap-2">
				<a href="{base}/edit" class={button}>Edit</a>
				{#if data.monitor.paused}
					<form method="POST" action="?/resume" use:enhance={submit}>
						<button type="submit" disabled={busy} class={button}>Resume</button>
					</form>
				{:else}
					<form method="POST" action="?/pause" use:enhance={submit}>
						<button type="submit" disabled={busy} class={button}>Pause</button>
					</form>
				{/if}
				<form method="POST" action="?/delete" use:enhance={confirmDelete}>
					<button type="submit" disabled={busy} class="btn btn-danger">Delete</button>
				</form>
			</div>
		</div>
		{#if form?.message}
			<div role="alert" class="alert alert-danger mt-4">
				{form.message}
			</div>
		{/if}
		<!-- The second step of a delete without JavaScript (see the delete action). -->
		{#if form?.confirmDelete}
			<form
				method="POST"
				action="?/delete"
				use:enhance={submit}
				class="alert alert-danger mt-4 flex flex-wrap items-center gap-3"
			>
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">Delete monitor "{data.monitor.name}"? Its incidents are deleted too.</span>
				<button type="submit" disabled={busy} class="btn btn-danger-solid btn-sm">Yes, delete</button>
				<a href={base} class="text-ink-2 hover:text-ink">Cancel</a>
			</form>
		{/if}
	</section>

	<section aria-label="Uptime and response summary" class="grid grid-cols-2 gap-3 sm:gap-4 lg:grid-cols-4">
		{#each tiles as tile (tile.label)}
			<StatCard label={tile.label} value={tile.value} spark={tile.spark} sparkTone={tile.tone} />
		{/each}
	</section>

	<AvailabilityCard availability={data.availability} exportBase="{base}/checks.csv" />

	<Card title="Response time" subtitle="Last 24 hours, average per 30 minutes (UTC)">
		{#if data.stats}
			<ResponseTimeChart series={data.stats.series} />
		{:else}
			<p class="text-sm text-muted">Check history is unavailable right now.</p>
		{/if}
	</Card>

	<div class="grid gap-6 xl:grid-cols-5">
		<Card title="Recent checks" titleTag="h3" padded={false} class="min-w-0 xl:col-span-3">
			{#if data.checks.length === 0}
				<EmptyState icon="clock" message="No checks yet." />
			{:else}
				<div class="overflow-x-auto">
					<table class="table table-compact">
						<thead>
							<tr>
								<th>Time</th>
								<th>Result</th>
								<th>Status code</th>
								<th>Response</th>
								<th>Error</th>
							</tr>
						</thead>
						<tbody class="tabular-nums">
							<!-- Unkeyed on purpose: rows hold no state, and `checked_at` is not a
							     key (millisecond resolution, no unique constraint), so a repeated
							     timestamp must not become a duplicate-key error. -->
							{#each data.checks as check}
								<tr>
									<td class="whitespace-nowrap text-muted">{formatUtc(check.checked_at)}</td>
									<td><StatusBadge status={check.status} /></td>
									<td>{check.status_code ?? '—'}</td>
									<td class="whitespace-nowrap">{formatMs(check.response_time_ms)}</td>
									<td class="text-muted wrap-anywhere">{check.error ?? ''}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</Card>

		<Card title="Incidents" titleTag="h3" padded={false} class="min-w-0 xl:col-span-2">
			{#if data.incidents.length === 0}
				<EmptyState icon="incident" message="No incidents." />
			{:else}
				<ul class="divide-y divide-border">
					{#each data.incidents as incident (incident.id)}
						<li class="flex flex-wrap items-baseline justify-between gap-2 px-5 py-3 sm:px-6">
							<div class="min-w-0">
								<!-- The cause can echo the keyword or text from the target, with no
								     spaces to wrap at: break anywhere instead of widening the page. -->
								<p class="text-sm wrap-anywhere text-ink">{incident.cause ?? incident.title}</p>
								<p class="text-xs text-muted">Started {formatUtc(incident.started_at)}</p>
							</div>
							<span class="badge {incident.status === 'open' ? 'badge-danger' : 'badge-neutral'}">
								{incident.status === 'open' ? 'Ongoing' : 'Resolved'} · {formatDuration(
									incident.started_at,
									incident.resolved_at,
									now
								)}
							</span>
						</li>
					{/each}
				</ul>
			{/if}
		</Card>
	</div>
</div>
