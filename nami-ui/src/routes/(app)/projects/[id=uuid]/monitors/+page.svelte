<script lang="ts">
	import { refreshAll } from '$app/navigation';
	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import Card from '#lib/components/ui/Card.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import Icon from '#lib/components/ui/Icon.svelte';
	import StatCard from '#lib/components/ui/StatCard.svelte';
	import StatStrip from '#lib/components/ui/StatStrip.svelte';
	import { CHECK_TYPE_LABELS, formatUptime, groupMonitors, monitorTarget, timeAgo } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const sections = $derived(groupMonitors(data.monitors, data.groups));
	const grouped = $derived(data.groups.length > 0);

	// KPI strip, from the monitors already loaded (no extra engine call).
	const kpi = $derived.by(() => {
		const total = data.monitors.length;
		const active = data.monitors.filter((m) => !m.paused);
		const up = active.filter((m) => m.current_status === 'up').length;
		const down = active.filter((m) => m.current_status === 'down').length;
		const paused = total - active.length;
		const maintenance = data.monitors.filter((m) => m.in_maintenance).length;
		const ratios = data.monitors.map((m) => m.uptime_24h).filter((r): r is number => r !== null);
		const avgUptime = ratios.length ? ratios.reduce((a, b) => a + b, 0) / ratios.length : null;
		const share = (n: number) => (total ? `${Math.round((n / total) * 100)}%` : '0%');
		return { total, up, down, paused, maintenance, avgUptime, share };
	});

	// Uptime meter colour: AA-safe fills; the percentage is always printed too.
	function meterTone(ratio: number | null): string {
		if (ratio === null) return 'bg-border';
		if (ratio >= 0.99) return 'bg-success';
		if (ratio >= 0.95) return 'bg-warning';
		return 'bg-danger';
	}

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
		<!-- Fixed layout with one shared colgroup: the columns line up across
		     the group tables. -->
		<table class="table table-hover min-w-[56rem] table-fixed">
			<colgroup>
				<col class="w-[11rem]" />
				<col class="w-[14rem]" />
				<col class="w-[6.5rem]" />
				<col />
				<col class="w-[12rem]" />
				<col class="w-[8.5rem]" />
			</colgroup>
			<thead>
				<tr>
					<th>Status</th>
					<th>Name</th>
					<th>Type</th>
					<th>Target</th>
					<th>Uptime (24 h)</th>
					<th>Last checked</th>
				</tr>
			</thead>
			<tbody>
				{#each monitors as monitor (monitor.id)}
					<tr>
						<td>
							<div class="flex flex-wrap items-center gap-2">
								<StatusBadge status={monitor.current_status} paused={monitor.paused} />
								{#if monitor.in_maintenance}<MaintenanceBadge />{/if}
							</div>
						</td>
						<td>
							<a
								href="/projects/{data.project.id}/monitors/{monitor.id}"
								title={monitor.name}
								class="block truncate font-medium text-ink hover:text-primary">{monitor.name}</a
							>
						</td>
						<td class="whitespace-nowrap">{CHECK_TYPE_LABELS[monitor.check_type]}</td>
						<td class="truncate font-mono text-xs" title={monitorTarget(monitor)}>
							{monitorTarget(monitor)}
						</td>
						<td>
							<div class="flex items-center gap-3">
								<span class="w-16 text-ink tabular-nums">{formatUptime(monitor.uptime_24h)}</span>
								<span class="hidden h-1.5 w-20 overflow-hidden rounded-full bg-border sm:block" aria-hidden="true">
									<span
										class="block h-full rounded-full {meterTone(monitor.uptime_24h)}"
										style:width="{Math.max(0, Math.min(1, monitor.uptime_24h ?? 0)) * 100}%"
									></span>
								</span>
							</div>
						</td>
						<td class="whitespace-nowrap text-muted">{timeAgo(monitor.last_checked_at, now)}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
{/snippet}

<div class="space-y-6">
	{#if data.monitors.length > 0}
		<!-- Shares are a snapshot, not a trend: plain hint text, no arrows. -->
		<StatStrip label="Monitor summary" cols={5}>
			<StatCard label="Monitors" value={String(kpi.total)} hint="{data.groups.length} group{data.groups.length === 1 ? '' : 's'}" />
			<StatCard
				label="Up"
				value={String(kpi.up)}
				valueTone={kpi.up > 0 ? 'success' : undefined}
				hint="{kpi.share(kpi.up)} of all monitors"
			/>
			<StatCard
				label="Down"
				value={String(kpi.down)}
				valueTone={kpi.down > 0 ? 'danger' : undefined}
				hint="{kpi.share(kpi.down)} of all monitors"
			/>
			<StatCard
				label="Paused"
				value={String(kpi.paused)}
				hint="{kpi.maintenance} in maintenance"
			/>
			<StatCard label="Avg uptime, 24 h" value={formatUptime(kpi.avgUptime)} hint="across monitors with checks" />
		</StatStrip>
	{/if}

	<Card title="Monitors" subtitle="Refreshes every 30 seconds." padded={false}>
		{#snippet actions()}
			<a href="/projects/{data.project.id}/monitors/import" class="btn btn-secondary">Import</a>
			<a href="/projects/{data.project.id}/monitors/new" class="btn btn-primary">
				<Icon name="plus" size={16} /> New monitor
			</a>
		{/snippet}

		{#if data.monitors.length === 0}
			<EmptyState icon="monitor" message="No monitors yet — create one to start checking." />
		{:else if !grouped}
			{@render table(data.monitors)}
		{:else}
			<!-- Native <details>: collapsing needs no script, and the keyed each
			     keeps each group's open/closed state across the 30 s refresh. -->
			{#each sections as section (section.group?.id ?? 'ungrouped')}
				{@const down = section.monitors.filter((m) => m.current_status === 'down' && !m.paused).length}
				<details open class="group/section border-b border-border last:border-0">
					<summary
						class="flex cursor-pointer list-none items-center gap-3 bg-surface-2 px-5 py-3 text-sm hover:bg-primary-soft sm:px-6 [&::-webkit-details-marker]:hidden"
					>
						<Icon name="chevronDown" size={16} class="-rotate-90 text-muted transition-transform group-open/section:rotate-0" />
						<span class="font-medium wrap-anywhere text-ink">{section.group?.name ?? 'Ungrouped'}</span>
						<span class="text-xs text-muted">
							{section.monitors.length} monitor{section.monitors.length === 1 ? '' : 's'}
						</span>
						{#if down > 0}<span class="badge badge-danger">{down} down</span>{/if}
					</summary>
					{@render table(section.monitors)}
				</details>
			{/each}
		{/if}
	</Card>
</div>
