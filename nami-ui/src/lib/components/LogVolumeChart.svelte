<script lang="ts">
	// Log volume per bucket, stacked by severity: errors (status critical,
	// #d03b3b) at the baseline, then warnings (status warning, #fab219), then
	// everything else in neutral #737373 (3.9:1 on the #141414 card). Status
	// steps from the dataviz reference palette (deutan/tritan ΔE 24 apart);
	// identity is never color alone: legend, tooltip and table name each part.
	// A click on a bar zooms the explorer into that bucket; keyboard users get
	// the same through the range input (arrows to inspect, Enter to zoom),
	// hence the a11y ignore on the chart's click handler.
	import { formatUtc, niceTicks } from '#lib/uptime';
	import { bucketEnd, bucketParts, type Histogram, type HistogramBucket } from '#lib/logs';

	let {
		histogram,
		onselect
	}: { histogram: Histogram; onselect?: (from: string, to: string) => void } = $props();

	const HEIGHT = 160;
	const PAD = { top: 8, right: 8, bottom: 24, left: 48 };
	const PLOT_HEIGHT = HEIGHT - PAD.top - PAD.bottom;
	const GAP = 2;
	const SERIES = [
		{ key: 'errors', label: 'Errors', fill: 'fill-[#d03b3b]', swatch: 'bg-[#d03b3b]' },
		{ key: 'warnings', label: 'Warnings', fill: 'fill-[#fab219]', swatch: 'bg-[#fab219]' },
		{ key: 'other', label: 'Other', fill: 'fill-[#737373]', swatch: 'bg-[#737373]' }
	] as const;

	let width = $state(720);
	let hoverIndex = $state<number | null>(null);
	let focusIndex = $state<number | null>(null);

	const buckets = $derived(histogram.buckets);
	const plotWidth = $derived(Math.max(width - PAD.left - PAD.right, 1));
	const band = $derived(plotWidth / Math.max(buckets.length, 1));
	const barWidth = $derived(Math.max(1, Math.min(24, band - GAP)));
	const ticks = $derived(niceTicks(Math.max(0, ...buckets.map((b) => b.count)), 3));
	const yMax = $derived(Math.max(ticks[ticks.length - 1], 1));
	const active = $derived(hoverIndex ?? focusIndex);
	const stepMs = $derived(histogram.step_seconds * 1000);
	const labelEvery = $derived(Math.max(1, Math.ceil(buckets.length / (plotWidth < 420 ? 3 : 6))));

	const parts = bucketParts;

	function yAt(value: number): number {
		return PAD.top + PLOT_HEIGHT - (value / yMax) * PLOT_HEIGHT;
	}

	/** Stacked segments, bottom up, with a surface gap between them. */
	function segments(bucket: HistogramBucket, i: number) {
		const x = PAD.left + i * band + (band - barWidth) / 2;
		const values = parts(bucket);
		const present = SERIES.filter((series) => values[series.key] > 0);
		let base = PAD.top + PLOT_HEIGHT;
		return present.map((series, k) => {
			const full = (values[series.key] / yMax) * PLOT_HEIGHT;
			const gap = k < present.length - 1 ? Math.min(GAP, full / 2) : 0;
			const height = Math.max(full - gap, 1);
			const top = base - height;
			const segment = { key: series.key, fill: series.fill, x, y: top, height, last: k === present.length - 1 };
			base = top - gap;
			return segment;
		});
	}

	/** A rect with only its top corners rounded (data end), square at the base. */
	function roundedTop(x: number, y: number, w: number, h: number): string {
		const r = Math.min(4, w / 2, h);
		return `M${x},${y + h} V${y + r} Q${x},${y} ${x + r},${y} H${x + w - r} Q${x + w},${y} ${x + w},${y + r} V${y + h} Z`;
	}

	function label(iso: string): string {
		const text = new Date(iso).toISOString();
		const span = buckets.length * stepMs;
		if (histogram.step_seconds >= 86_400) return text.slice(5, 10);
		return span > 86_400_000 ? `${text.slice(5, 10)} ${text.slice(11, 16)}` : text.slice(11, 16);
	}

	function describe(i: number): string {
		const bucket = buckets[i];
		if (!bucket) return '';
		const p = parts(bucket);
		return `${formatUtc(bucket.start)}: ${bucket.count} logs (${p.errors} errors, ${p.warnings} warnings, ${p.other} other)`;
	}

	function onPointerMove(event: PointerEvent) {
		const box = (event.currentTarget as SVGSVGElement).getBoundingClientRect();
		const index = Math.floor((event.clientX - box.left - PAD.left) / band);
		hoverIndex = index >= 0 && index < buckets.length ? index : null;
	}

	function select(i: number | null) {
		if (i === null || !onselect || !buckets[i]) return;
		onselect(new Date(buckets[i].start).toISOString(), bucketEnd(buckets[i], histogram.step_seconds));
	}
</script>

<div class="space-y-2">
	<div class="flex flex-wrap items-center gap-4 text-xs text-neutral-400">
		{#each SERIES as series (series.key)}
			<span class="flex items-center gap-1.5"><span class="h-2.5 w-2.5 rounded-sm {series.swatch}"></span>{series.label}</span>
		{/each}
	</div>
	<div class="relative" bind:clientWidth={width}>
		<label class="sr-only" for="log-volume-inspector">Inspect log volume by time bucket</label>
		<input
			id="log-volume-inspector"
			class="peer sr-only"
			type="range"
			min="0"
			max={Math.max(buckets.length - 1, 0)}
			step="1"
			value={focusIndex ?? Math.max(buckets.length - 1, 0)}
			aria-valuetext={describe(focusIndex ?? buckets.length - 1)}
			oninput={(event) => (focusIndex = Number(event.currentTarget.value))}
			onfocus={(event) => (focusIndex = Number(event.currentTarget.value))}
			onblur={() => (focusIndex = null)}
			onkeydown={(event) => {
				if (event.key === 'Enter') select(focusIndex);
			}}
		/>
		<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
		<svg
			{width}
			height={HEIGHT}
			role="img"
			aria-label="Log volume over the selected time range, stacked by errors, warnings and other levels"
			class="block cursor-pointer rounded-md peer-focus-visible:ring-2 peer-focus-visible:ring-teal-500/50"
			onpointermove={onPointerMove}
			onpointerleave={() => (hoverIndex = null)}
			onclick={() => select(hoverIndex)}
		>
			{#each ticks as tick (tick)}
				<line x1={PAD.left} x2={PAD.left + plotWidth} y1={yAt(tick)} y2={yAt(tick)} class="stroke-[#262626]" stroke-width="1" />
				<text x={PAD.left - 8} y={yAt(tick)} dy="0.32em" text-anchor="end" class="fill-neutral-500 text-[11px] tabular-nums"
					>{tick}</text
				>
			{/each}
			{#each buckets as bucket, i (bucket.start)}
				{#if active === i}
					<rect x={PAD.left + i * band} y={PAD.top} width={band} height={PLOT_HEIGHT} class="fill-neutral-800/60" />
				{/if}
				{#each segments(bucket, i) as segment (segment.key)}
					{#if segment.last}
						<path d={roundedTop(segment.x, segment.y, barWidth, segment.height)} class={segment.fill} />
					{:else}
						<rect x={segment.x} y={segment.y} width={barWidth} height={segment.height} class={segment.fill} />
					{/if}
				{/each}
				{#if i % labelEvery === 0}
					<text x={PAD.left + i * band + band / 2} y={HEIGHT - 8} text-anchor="middle" class="fill-neutral-500 text-[11px] tabular-nums"
						>{label(bucket.start)}</text
					>
				{/if}
			{/each}
		</svg>

		{#if histogram.total === 0}
			<p class="pointer-events-none absolute inset-0 flex items-center justify-center text-sm text-neutral-500">
				No logs in this time range.
			</p>
		{/if}

		{#if active !== null && buckets[active]}
			{@const bucket = buckets[active]}
			{@const p = parts(bucket)}
			<div
				class="pointer-events-none absolute top-2 w-48 rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-xs shadow-lg"
				style:left="{Math.max(0, Math.min(PAD.left + active * band + band + 8, width - 200))}px"
			>
				<div class="text-sm font-semibold text-neutral-100 tabular-nums">{bucket.count.toLocaleString('en-US')} logs</div>
				<div class="text-neutral-400 tabular-nums">{p.errors} errors · {p.warnings} warnings · {p.other} other</div>
				<div class="text-neutral-500">{formatUtc(bucket.start)}</div>
				{#if onselect}<div class="text-neutral-600">Click to zoom in</div>{/if}
			</div>
		{/if}
	</div>
	<details class="text-sm">
		<summary class="cursor-pointer text-neutral-500 hover:text-neutral-300">Show data table</summary>
		<div class="mt-2 max-h-64 overflow-y-auto">
			<table class="w-full text-left text-xs">
				<thead class="text-neutral-500">
					<tr>
						<th class="py-1 font-medium">Bucket start (UTC)</th>
						<th class="py-1 font-medium">Logs</th>
						<th class="py-1 font-medium">Errors</th>
						<th class="py-1 font-medium">Warnings</th>
					</tr>
				</thead>
				<tbody class="text-neutral-300 tabular-nums">
					{#each buckets as bucket (bucket.start)}
						<tr class="border-t border-[#262626]">
							<td class="py-1">{formatUtc(bucket.start)}</td>
							<td class="py-1">{bucket.count}</td>
							<td class="py-1">{bucket.errors}</td>
							<td class="py-1">{bucket.warnings}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	</details>
</div>
