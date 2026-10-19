<script lang="ts">
	// Availability (SLA) for a monitor or heartbeat over a range: uptime,
	// downtime, incidents and the longest incident, with maintenance time left
	// out. The range lives in the URL (`?range=7d`, or `?range=custom&from&to`),
	// so the tabs are plain links and the card works without JavaScript.
	import { exportRange, formatPct, RANGE_PRESETS, type Availability, type SelectedRange } from '#lib/availability';
	import type { AvailabilityView } from '#lib/server/availability';
	import { formatSeconds } from '#lib/uptime';
	import CardMenu from './ui/CardMenu.svelte';
	import Card from './ui/Card.svelte';
	import SegmentedTabs from './ui/SegmentedTabs.svelte';
	import StatCard from './ui/StatCard.svelte';
	import StatStrip from './ui/StatStrip.svelte';

	let {
		availability,
		exportBase
	}: {
		availability: AvailabilityView;
		/** The CSV route of a monitor (`…/checks.csv`); no export for a heartbeat. */
		exportBase?: string;
	} = $props();

	const range: SelectedRange = $derived(availability.range);
	const data: Availability | null = $derived(availability.data);

	const tabItems = $derived([
		...RANGE_PRESETS.map((p) => ({ href: `?range=${p.key}`, label: p.label, active: range.key === p.key })),
		{ href: '?range=custom', label: 'Custom', active: range.key === 'custom' }
	]);

	const exporting = $derived(exportBase ? exportRange(range) : null);
	const exportHref = $derived(
		exportBase && exporting
			? `${exportBase}?from=${encodeURIComponent(exporting.from)}&to=${encodeURIComponent(exporting.to)}`
			: null
	);

	const noData = $derived(data !== null && data.uptime_pct === null);
	const uptime = $derived(data ? formatPct(data.uptime_pct) : '—');
	const uptimeTone = $derived(
		data === null || data.uptime_pct === null ? undefined : data.uptime_pct >= 99.9 ? ('success' as const) : data.uptime_pct >= 99 ? ('warning' as const) : ('danger' as const)
	);
	const utc = (iso: string) => iso.slice(0, 16).replace('T', ' ') + ' UTC';
</script>

<Card
	title="Availability"
	subtitle={data ? `${utc(data.from)} to ${utc(data.to)}` : 'Uptime over a period'}
	titleTag="h3"
>
	{#snippet tabs()}
		<SegmentedTabs items={tabItems} label="Availability range" size="sm" />
	{/snippet}
	{#snippet menu()}
		{#if exportHref}
			<CardMenu label="Availability actions">
				<a class="menu-item" href={exportHref}>Export CSV</a>
			</CardMenu>
		{/if}
	{/snippet}

	<div class="space-y-4">
		{#if range.key === 'custom'}
			<form method="GET" class="flex flex-wrap items-end gap-3">
				<input type="hidden" name="range" value="custom" />
				<div>
					<label class="label" for="availability-from">From</label>
					<input id="availability-from" class="input" type="date" name="from" value={range.customFrom} required />
				</div>
				<div>
					<label class="label" for="availability-to">To (included)</label>
					<input id="availability-to" class="input" type="date" name="to" value={range.customTo} required />
				</div>
				<button type="submit" class="btn btn-primary">Show</button>
			</form>
			{#if range.error}
				<p role="alert" class="alert alert-danger">{range.error}</p>
			{/if}
		{/if}

		{#if availability.unavailable}
			<p class="text-sm text-muted">Availability is unavailable right now.</p>
		{:else if data === null}
			<p class="text-sm text-muted">Pick a start and an end date.</p>
		{:else}
			<StatStrip cols={4} label="Availability for the selected range">
				<StatCard
					label="Uptime"
					value={uptime}
					unit={noData ? undefined : '%'}
					valueTone={uptimeTone}
					hint={noData ? 'No checks in this range' : undefined}
				/>
				<StatCard
					label="Downtime"
					value={formatSeconds(data.downtime_seconds)}
					hint={data.maintenance_seconds > 0 ? `${formatSeconds(data.maintenance_seconds)} maintenance excluded` : 'No maintenance'}
				/>
				<StatCard label="Incidents" value={String(data.incidents)} />
				<StatCard label="Longest incident" value={data.incidents > 0 ? formatSeconds(data.longest_incident_seconds) : '—'} />
			</StatStrip>
		{/if}

		<p class="text-xs text-muted">
			Incident time is confirmed downtime, so it can differ from the check-based uptime tiles.
			Planned maintenance is left out of both uptime and downtime.
			{#if exporting?.clipped}
				The CSV covers the last 180 days of this range.
			{/if}
		</p>
	</div>
</Card>
