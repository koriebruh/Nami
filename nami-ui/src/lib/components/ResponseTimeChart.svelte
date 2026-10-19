<script lang="ts">
	// Average response time per 30-minute bucket over the last 24 hours.
	// One series in the primary token over a soft gradient area, with a dot
	// legend under the plot; gaps (buckets without checks) break the line
	// instead of being interpolated.
	import Legend from '#lib/components/ui/Legend.svelte';
	import {
		formatClockUtc,
		formatMs,
		formatUptime,
		formatUtc,
		niceTicks,
		type SeriesBucket
	} from '#lib/uptime';

	let { series }: { series: SeriesBucket[] } = $props();

	const HEIGHT = 220;
	const PAD = { top: 16, right: 72, bottom: 28, left: 56 };
	const PLOT_HEIGHT = HEIGHT - PAD.top - PAD.bottom;
	const BUCKET_MS = 30 * 60 * 1000;

	let width = $state(720);
	let hoverIndex = $state<number | null>(null);
	let focusIndex = $state<number | null>(null);

	const plotWidth = $derived(Math.max(width - PAD.left - PAD.right, 1));
	// Time labels every 6 hours, or every 12 on narrow screens so they never
	// collide with each other or with the "now" label.
	const labelEvery = $derived(plotWidth < 420 ? 24 : 12);
	const points = $derived(
		series.flatMap((bucket, i) =>
			bucket.avg_response_ms === null ? [] : [{ i, v: bucket.avg_response_ms }]
		)
	);
	const ticks = $derived(niceTicks(Math.max(0, ...points.map((p) => p.v))));
	const yMax = $derived(ticks[ticks.length - 1]);
	const last = $derived(points.length > 0 ? points[points.length - 1] : null);
	const active = $derived(hoverIndex ?? focusIndex);

	function xAt(i: number): number {
		return PAD.left + (series.length > 1 ? (i / (series.length - 1)) * plotWidth : plotWidth / 2);
	}

	function yAt(v: number): number {
		return PAD.top + PLOT_HEIGHT - (v / yMax) * PLOT_HEIGHT;
	}

	// Runs of consecutive buckets with data: each run is one path, a run of
	// one bucket is drawn as a dot.
	const runs = $derived.by(() => {
		const result: { i: number; v: number }[][] = [];
		let run: { i: number; v: number }[] = [];
		series.forEach((bucket, i) => {
			if (bucket.avg_response_ms === null) {
				if (run.length > 0) result.push(run);
				run = [];
			} else {
				run.push({ i, v: bucket.avg_response_ms });
			}
		});
		if (run.length > 0) result.push(run);
		return result;
	});

	function pathFor(run: { i: number; v: number }[]): string {
		return run
			.map((p, k) => `${k === 0 ? 'M' : 'L'}${xAt(p.i).toFixed(1)},${yAt(p.v).toFixed(1)}`)
			.join(' ');
	}

	// The area under a run, closed along the x axis.
	function areaFor(run: { i: number; v: number }[]): string {
		const floor = (PAD.top + PLOT_HEIGHT).toFixed(1);
		return `${pathFor(run)} L${xAt(run[run.length - 1].i).toFixed(1)},${floor} L${xAt(run[0].i).toFixed(1)},${floor} Z`;
	}

	function bucketRange(bucket: SeriesBucket): string {
		const end = new Date(Date.parse(bucket.bucket_start) + BUCKET_MS).toISOString();
		return `${formatClockUtc(bucket.bucket_start)}–${formatClockUtc(end)} UTC`;
	}

	function describe(i: number): string {
		const bucket = series[i];
		if (!bucket) return '';
		return `${bucketRange(bucket)}: ${formatMs(bucket.avg_response_ms)} average, ${formatUptime(bucket.up_ratio)} up`;
	}

	// The crosshair snaps to the nearest bucket.
	function onPointerMove(event: PointerEvent) {
		const box = (event.currentTarget as SVGSVGElement).getBoundingClientRect();
		const ratio = (event.clientX - box.left - PAD.left) / plotWidth;
		hoverIndex = Math.min(series.length - 1, Math.max(0, Math.round(ratio * (series.length - 1))));
	}
</script>

<div class="relative" bind:clientWidth={width}>
	<!-- Keyboard access: arrow keys move through the buckets and show the
	     same readout as hovering; screen readers get it as the value text. -->
	<label class="sr-only" for="response-time-inspector">Inspect response time by half hour</label>
	<input
		id="response-time-inspector"
		class="peer sr-only"
		type="range"
		min="0"
		max={Math.max(series.length - 1, 0)}
		step="1"
		value={focusIndex ?? Math.max(series.length - 1, 0)}
		aria-valuetext={describe(focusIndex ?? series.length - 1)}
		oninput={(event) => (focusIndex = Number(event.currentTarget.value))}
		onfocus={(event) => (focusIndex = Number(event.currentTarget.value))}
		onblur={() => (focusIndex = null)}
	/>
	<svg
		{width}
		height={HEIGHT}
		role="img"
		aria-label="Average response time per 30 minutes over the last 24 hours"
		class="block rounded-md peer-focus-visible:ring-2 peer-focus-visible:ring-primary"
		onpointermove={onPointerMove}
		onpointerleave={() => (hoverIndex = null)}
	>
		<defs>
			<linearGradient id="response-time-area" x1="0" x2="0" y1="0" y2="1">
				<stop offset="0%" stop-color="var(--color-primary)" stop-opacity="0.18" />
				<stop offset="100%" stop-color="var(--color-accent-cyan)" stop-opacity="0.02" />
			</linearGradient>
		</defs>
		{#each ticks as tick (tick)}
			<line
				x1={PAD.left}
				x2={PAD.left + plotWidth}
				y1={yAt(tick)}
				y2={yAt(tick)}
				class="stroke-border"
				stroke-width="1"
			/>
			<text
				x={PAD.left - 8}
				y={yAt(tick)}
				dy="0.32em"
				text-anchor="end"
				class="fill-muted text-[11px] tabular-nums">{tick} ms</text
			>
		{/each}

		{#each series as bucket, i (bucket.bucket_start)}
			{#if i % labelEvery === 0}
				<text
					x={xAt(i)}
					y={HEIGHT - 8}
					text-anchor="middle"
					class="fill-muted text-[11px] tabular-nums">{formatClockUtc(bucket.bucket_start)}</text
				>
			{/if}
		{/each}
		<text x={PAD.left + plotWidth} y={HEIGHT - 8} text-anchor="end" class="fill-muted text-[11px]"
			>{labelEvery === 12 ? 'now (UTC)' : 'now'}</text
		>

		{#each runs as run (run[0].i)}
			{#if run.length === 1}
				<circle cx={xAt(run[0].i)} cy={yAt(run[0].v)} r="4" class="fill-primary" />
			{:else}
				<path d={areaFor(run)} fill="url(#response-time-area)" />
				<path
					d={pathFor(run)}
					fill="none"
					class="stroke-primary"
					stroke-width="2"
					stroke-linejoin="round"
					stroke-linecap="round"
				/>
			{/if}
		{/each}

		{#if active !== null && series[active]}
			<line
				x1={xAt(active)}
				x2={xAt(active)}
				y1={PAD.top}
				y2={PAD.top + PLOT_HEIGHT}
				class="stroke-faint"
				stroke-width="1"
			/>
		{/if}

		{#if last}
			<!-- End marker: an 8px dot (r=4 visible) inside a 2px surface ring. An
			     SVG stroke is centred on the circle's edge and painted over the
			     fill, so the circle is r=5: the stroke covers r=4..6 and leaves
			     the r=4 fill visible. -->
			<circle
				cx={xAt(last.i)}
				cy={yAt(last.v)}
				r="5"
				class="fill-primary stroke-surface"
				stroke-width="2"
			/>
			<!-- The latest value is the chart's only direct label. -->
			<text
				x={xAt(last.i) + 10}
				y={yAt(last.v)}
				dy="0.32em"
				class="fill-ink text-xs font-medium tabular-nums">{formatMs(last.v)}</text
			>
		{/if}
	</svg>

	{#if points.length === 0}
		<p class="pointer-events-none absolute inset-0 flex items-center justify-center text-sm text-muted">
			{series.some((bucket) => bucket.up_ratio !== null)
				? 'No response times in the last 24 hours: every check failed before a response arrived.'
				: 'No checks in the last 24 hours yet.'}
		</p>
	{/if}

	{#if active !== null && series[active]}
		{@const bucket = series[active]}
		<div
			class="pointer-events-none absolute top-2 w-52 rounded-card border border-border bg-surface px-3 py-2 text-xs shadow-pop"
			style:left="{Math.max(0, Math.min(xAt(active) + 12, width - 216))}px"
		>
			<div class="text-sm font-semibold text-ink tabular-nums">
				{formatMs(bucket.avg_response_ms)}
			</div>
			<div class="text-ink-2">avg response · {formatUptime(bucket.up_ratio)} up</div>
			<div class="text-muted">{bucketRange(bucket)}</div>
		</div>
	{/if}
</div>

<Legend class="mt-2" items={[{ label: 'Avg response time', tone: 'primary' }]} />

<details class="mt-3 text-sm">
	<summary class="summary">Show data table</summary>
	<div class="mt-2 max-h-64 overflow-y-auto">
		<table class="w-full text-left text-xs">
			<thead class="text-muted">
				<tr>
					<th class="py-1 font-medium">Half hour (UTC)</th>
					<th class="py-1 font-medium">Avg response</th>
					<th class="py-1 font-medium">Uptime</th>
				</tr>
			</thead>
			<tbody class="text-ink-2 tabular-nums">
				{#each series as bucket (bucket.bucket_start)}
					<tr class="border-t border-border">
						<td class="py-1">{formatUtc(bucket.bucket_start)}</td>
						<td class="py-1">{formatMs(bucket.avg_response_ms)}</td>
						<td class="py-1">{formatUptime(bucket.up_ratio)}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
</details>
