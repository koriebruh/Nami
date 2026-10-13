<script lang="ts">
	import { enhance } from '$app/forms';
	import { refreshAll } from '$app/navigation';
	import ResponseTimeChart from '#lib/components/ResponseTimeChart.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import {
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
			? { text: 'PAUSED', class: 'text-neutral-400' }
			: data.monitor.current_status === 'up'
				? { text: 'UP', class: 'text-teal-300' }
				: data.monitor.current_status === 'down'
					? { text: 'DOWN', class: 'text-red-400' }
					: { text: 'PENDING', class: 'text-neutral-400' }
	);
	const tiles = $derived([
		{ label: 'Uptime, 24 hours', value: formatUptime(data.stats?.uptime_24h) },
		{ label: 'Uptime, 7 days', value: formatUptime(data.stats?.uptime_7d) },
		{ label: 'Uptime, 30 days', value: formatUptime(data.stats?.uptime_30d) },
		{ label: 'Avg response, 24 hours', value: formatMs(data.stats?.avg_response_ms_24h) }
	]);

	const button =
		'rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100';
</script>

<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div>
				<p class="text-3xl font-semibold tracking-tight {headline.class}">{headline.text}</p>
				<!-- The engine stores no pause time, and `since` is when the status
				     before the pause began, so a paused monitor shows no "since". -->
				<p class="mt-1 text-sm text-neutral-500">
					{#if data.monitor.paused}
						Checks are paused.
					{:else}
						since {formatUtc(since)} ({timeAgo(since, now)})
					{/if}
				</p>
				<h2 class="mt-4 text-base font-semibold wrap-anywhere text-neutral-100">{data.monitor.name}</h2>
				<p class="font-mono text-xs break-all text-neutral-400">{monitorTarget(data.monitor)}</p>
				<p class="mt-1 text-xs text-neutral-500">
					Checked every {data.monitor.interval_seconds}s · last check {timeAgo(data.monitor.last_checked_at, now)}
				</p>
			</div>
			<div class="flex flex-wrap gap-2">
				<a href="{base}/edit" class={button}>Edit</a>
				{#if data.monitor.paused}
					<form method="POST" action="?/resume" use:enhance>
						<button type="submit" class={button}>Resume</button>
					</form>
				{:else}
					<form method="POST" action="?/pause" use:enhance>
						<button type="submit" class={button}>Pause</button>
					</form>
				{/if}
				<form
					method="POST"
					action="?/delete"
					use:enhance={({ cancel }) => {
						if (!confirm(`Delete monitor "${data.monitor.name}"? Its incidents are deleted too.`)) cancel();
					}}
				>
					<button
						type="submit"
						class="rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400"
					>
						Delete
					</button>
				</form>
			</div>
		</div>
		{#if form?.message}
			<div role="alert" class="mt-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				{form.message}
			</div>
		{/if}
	</div>

	<div class="grid grid-cols-2 gap-4 lg:grid-cols-4">
		{#each tiles as tile (tile.label)}
			<div class="rounded-xl border border-[#262626] bg-[#141414] p-4">
				<p class="text-xs text-neutral-500">{tile.label}</p>
				<p class="mt-1 text-xl font-semibold text-neutral-100">{tile.value}</p>
			</div>
		{/each}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Response time, last 24 hours</h3>
		<div class="mt-4">
			{#if data.stats}
				<ResponseTimeChart series={data.stats.series} />
			{:else}
				<p class="text-sm text-neutral-500">Check history is unavailable right now.</p>
			{/if}
		</div>
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<h3 class="border-b border-[#262626] px-6 py-4 text-sm font-semibold text-neutral-200">Recent checks</h3>
		{#if data.checks.length === 0}
			<p class="px-6 py-6 text-sm text-neutral-500">No checks yet.</p>
		{:else}
			<div class="overflow-x-auto">
				<table class="w-full text-sm">
					<thead class="text-left text-xs text-neutral-500">
						<tr class="border-b border-[#262626]">
							<th class="px-6 py-2 font-medium">Time</th>
							<th class="px-6 py-2 font-medium">Result</th>
							<th class="px-6 py-2 font-medium">Status code</th>
							<th class="px-6 py-2 font-medium">Response</th>
							<th class="px-6 py-2 font-medium">Error</th>
						</tr>
					</thead>
					<tbody class="tabular-nums">
						<!-- Unkeyed on purpose: rows hold no state, and `checked_at` is not a
						     key (millisecond resolution, no unique constraint), so a repeated
						     timestamp must not become a duplicate-key error. -->
						{#each data.checks as check}
							<tr class="border-b border-[#262626] last:border-0">
								<td class="px-6 py-2 whitespace-nowrap text-neutral-400">{formatUtc(check.checked_at)}</td>
								<td class="px-6 py-2"><StatusBadge status={check.status} /></td>
								<td class="px-6 py-2 text-neutral-300">{check.status_code ?? '—'}</td>
								<td class="px-6 py-2 text-neutral-300">{formatMs(check.response_time_ms)}</td>
								<td class="px-6 py-2 text-neutral-400">{check.error ?? ''}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<h3 class="border-b border-[#262626] px-6 py-4 text-sm font-semibold text-neutral-200">Incidents</h3>
		{#if data.incidents.length === 0}
			<p class="px-6 py-6 text-sm text-neutral-500">No incidents.</p>
		{:else}
			<ul>
				{#each data.incidents as incident (incident.id)}
					<li class="flex flex-wrap items-baseline justify-between gap-2 border-b border-[#262626] px-6 py-3 last:border-0">
						<div>
							<!-- The cause can echo the keyword or text from the target, with no
							     spaces to wrap at: break anywhere instead of widening the page. -->
							<p class="text-sm wrap-anywhere text-neutral-200">{incident.cause ?? incident.title}</p>
							<p class="text-xs text-neutral-500">Started {formatUtc(incident.started_at)}</p>
						</div>
						<p class="text-xs {incident.status === 'open' ? 'text-red-400' : 'text-neutral-500'}">
							{incident.status === 'open' ? 'Ongoing' : 'Resolved'} · {formatDuration(
								incident.started_at,
								incident.resolved_at,
								now
							)}
						</p>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
</div>
