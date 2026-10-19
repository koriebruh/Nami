<script lang="ts">
	import { refreshAll } from '$app/navigation';
	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import { CHECK_TYPE_LABELS, formatUptime, groupMonitors, monitorTarget, timeAgo } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const sections = $derived(groupMonitors(data.monitors, data.groups));
	const grouped = $derived(data.groups.length > 0);

	// Re-run the load every 30 s like BetterStack's dashboard, and tick a
	// clock so "last checked" stays current in between. `refreshAll` is Kit
	// 3's replacement for the deprecated `invalidateAll`.
	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});
</script>

<svelte:head>
	<title>Monitors · {data.project.name}</title>
</svelte:head>

{#snippet table(monitors: typeof data.monitors)}
	<div class="overflow-x-auto">
		<table class="w-full text-sm">
			<thead class="text-left text-xs text-neutral-500">
				<tr class="border-b border-[#262626]">
					<th class="px-6 py-2 font-medium">Status</th>
					<th class="px-6 py-2 font-medium">Name</th>
					<th class="px-6 py-2 font-medium">Type</th>
					<th class="px-6 py-2 font-medium">Target</th>
					<th class="px-6 py-2 font-medium">Uptime (24 h)</th>
					<th class="px-6 py-2 font-medium">Last checked</th>
				</tr>
			</thead>
			<tbody>
				{#each monitors as monitor (monitor.id)}
					<tr class="border-b border-[#262626] last:border-0 hover:bg-[#0a0a0a]">
						<td class="px-6 py-3">
							<div class="flex flex-wrap items-center gap-2">
								<StatusBadge status={monitor.current_status} paused={monitor.paused} />
								{#if monitor.in_maintenance}<MaintenanceBadge />{/if}
							</div>
						</td>
						<td class="px-6 py-3">
							<a
								href="/projects/{data.project.id}/monitors/{monitor.id}"
								class="font-medium text-neutral-100 hover:text-teal-300">{monitor.name}</a
							>
						</td>
						<td class="px-6 py-3 text-neutral-400">{CHECK_TYPE_LABELS[monitor.check_type]}</td>
						<td class="max-w-xs truncate px-6 py-3 font-mono text-xs text-neutral-400" title={monitorTarget(monitor)}>
							{monitorTarget(monitor)}
						</td>
						<td class="px-6 py-3 text-neutral-200 tabular-nums">{formatUptime(monitor.uptime_24h)}</td>
						<td class="px-6 py-3 text-neutral-500">{timeAgo(monitor.last_checked_at, now)}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
{/snippet}

<div class="rounded-xl border border-[#262626] bg-[#141414]">
	<div class="flex items-center justify-between border-b border-[#262626] px-6 py-4">
		<div>
			<h2 class="text-base font-semibold text-neutral-100">Monitors</h2>
			<p class="text-xs text-neutral-500">Refreshes every 30 seconds.</p>
		</div>
		<a
			href="/projects/{data.project.id}/monitors/new"
			class="rounded-md bg-teal-500 px-3 py-1.5 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400"
		>
			New monitor
		</a>
	</div>

		{#if data.monitors.length === 0}
		<p class="px-6 py-10 text-center text-sm text-neutral-500">No monitors yet — create one to start checking.</p>
	{:else if !grouped}
		{@render table(data.monitors)}
	{:else}
		<!-- Native <details>: collapsing needs no script, and the keyed each
		     keeps each group's open/closed state across the 30 s refresh. -->
		{#each sections as section (section.group?.id ?? 'ungrouped')}
			{@const down = section.monitors.filter((m) => m.current_status === 'down' && !m.paused).length}
			<details open class="border-b border-[#262626] last:border-0">
				<summary class="flex cursor-pointer items-center gap-3 px-6 py-3 text-sm hover:bg-[#0a0a0a]">
					<span class="font-medium wrap-anywhere text-neutral-200">{section.group?.name ?? 'Ungrouped'}</span>
					<span class="text-xs text-neutral-500">
						{section.monitors.length} monitor{section.monitors.length === 1 ? '' : 's'}
					</span>
					{#if down > 0}<span class="text-xs text-red-400">{down} down</span>{/if}
				</summary>
				{@render table(section.monitors)}
			</details>
		{/each}
	{/if}
</div>
