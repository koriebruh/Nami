<script lang="ts">
	// A source's daily ingestion over the last 30 days (UTC): events per day
	// in the primary token, bytes per day in the cyan accent, each row scaled
	// to its own maximum. Every bar has a tooltip, and a visually hidden table
	// repeats the data for screen readers.
	import Legend from '#lib/components/ui/Legend.svelte';
	import { formatBytes, usageBars, type DailyUsage } from '#lib/logs/logs';

	let { days }: { days: DailyUsage[] } = $props();

	const HEIGHT = 48;
	const BAND = 10;
	const BAR = 7;

	const bars = $derived(usageBars(days));
	const width = $derived(Math.max(bars.length, 1) * BAND);
	const maxEvents = $derived(Math.max(0, ...days.map((d) => d.events)));
	const maxBytes = $derived(Math.max(0, ...days.map((d) => d.bytes)));
	const rows = $derived([
		{ key: 'events', label: 'Events per day', max: maxEvents.toLocaleString('en-US'), fill: 'fill-primary' },
		{ key: 'bytes', label: 'Bytes per day', max: formatBytes(maxBytes), fill: 'fill-accent-cyan' }
	] as const);

	const count = (n: number) => n.toLocaleString('en-US');

	function tooltip(bar: (typeof bars)[number]): string {
		return `${bar.day} (UTC): ${count(bar.events)} events, ${formatBytes(bar.bytes)}; ${count(bar.dropped)} dropped, ${count(bar.sampledOut)} sampled out, ${count(bar.rejected)} requests rejected`;
	}

	/** At least a hairline for a non-zero day, so it never looks empty. */
	function barHeight(share: number): number {
		return share > 0 ? Math.max(share * HEIGHT, 1) : 0;
	}
</script>

<div class="space-y-3">
	<Legend
		items={[
			{ label: 'Events', tone: 'primary' },
			{ label: 'Bytes', tone: 'cyan' }
		]}
	/>
	{#each rows as row (row.key)}
		<div>
			<div class="flex items-baseline justify-between text-xs text-muted">
				<span>{row.label}</span>
				<span class="tabular-nums">max {row.max}</span>
			</div>
			<svg
				viewBox="0 0 {width} {HEIGHT}"
				preserveAspectRatio="none"
				class="mt-1 block h-12 w-full border-b border-border"
				aria-hidden="true"
			>
				{#each bars as bar, i (bar.day)}
					{@const share = row.key === 'events' ? bar.eventsShare : bar.bytesShare}
					<g>
						<title>{tooltip(bar)}</title>
						<!-- A full-height hit area, so a zero day still shows its tooltip. -->
						<rect x={i * BAND} y="0" width={BAND} height={HEIGHT} class="fill-transparent" />
						<rect
							x={i * BAND + (BAND - BAR) / 2}
							y={HEIGHT - barHeight(share)}
							width={BAR}
							height={barHeight(share)}
							class={row.fill}
						/>
					</g>
				{/each}
			</svg>
		</div>
	{/each}
	{#if bars.length > 0}
		<div class="flex justify-between text-xs text-muted tabular-nums" aria-hidden="true">
			<span>{bars[0].day}</span>
			<span>{bars[bars.length - 1].day}</span>
		</div>
	{/if}
	<table class="sr-only">
		<caption>Daily ingestion, last {bars.length} days (UTC)</caption>
		<thead>
			<tr>
				<th scope="col">Day (UTC)</th>
				<th scope="col">Events</th>
				<th scope="col">Bytes</th>
				<th scope="col">Dropped</th>
				<th scope="col">Sampled out</th>
				<th scope="col">Requests rejected</th>
			</tr>
		</thead>
		<tbody>
			{#each bars as bar (bar.day)}
				<tr>
					<th scope="row">{bar.day}</th>
					<td>{count(bar.events)}</td>
					<td>{formatBytes(bar.bytes)}</td>
					<td>{count(bar.dropped)}</td>
					<td>{count(bar.sampledOut)}</td>
					<td>{count(bar.rejected)}</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>
