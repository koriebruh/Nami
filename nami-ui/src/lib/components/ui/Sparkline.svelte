<script lang="ts">
	// A dependency-free SVG sparkline. Null values are gaps; the line breaks
	// there instead of interpolating. With fewer than two finite points there
	// is no trend to draw: a flat baseline is shown instead. Decorative
	// unless `label` is given.
	import { finitePoints, TONE_VAR, type Tone } from './tones';

	let {
		values,
		tone = 'primary',
		width = 88,
		height = 28,
		fill = true,
		label
	}: {
		values: (number | null)[];
		tone?: Tone;
		width?: number;
		height?: number;
		fill?: boolean;
		label?: string;
	} = $props();

	const PAD = 2;
	const uid = $props.id();
	const gradientId = `spark-${uid}`;

	const known = $derived(values.filter((v): v is number => v !== null && Number.isFinite(v)));
	const min = $derived(known.length ? Math.min(...known) : 0);
	const max = $derived(known.length ? Math.max(...known) : 1);

	function x(i: number): number {
		return values.length > 1 ? PAD + (i / (values.length - 1)) * (width - PAD * 2) : width / 2;
	}
	function y(v: number): number {
		const span = max - min || 1;
		// A flat series sits in the middle rather than on the floor.
		if (max === min) return height / 2;
		return PAD + (1 - (v - min) / span) * (height - PAD * 2);
	}

	const runs = $derived.by(() => {
		const out: { i: number; v: number }[][] = [];
		let run: { i: number; v: number }[] = [];
		values.forEach((v, i) => {
			if (v === null || !Number.isFinite(v)) {
				if (run.length) out.push(run);
				run = [];
			} else run.push({ i, v });
		});
		if (run.length) out.push(run);
		return out;
	});

	const line = (run: { i: number; v: number }[]) =>
		run.map((p, k) => `${k ? 'L' : 'M'}${x(p.i).toFixed(1)},${y(p.v).toFixed(1)}`).join(' ');
	const area = (run: { i: number; v: number }[]) =>
		`${line(run)} L${x(run[run.length - 1].i).toFixed(1)},${height} L${x(run[0].i).toFixed(1)},${height} Z`;
</script>

<svg
	{width}
	{height}
	viewBox="0 0 {width} {height}"
	class="block shrink-0 overflow-visible"
	role={label ? 'img' : undefined}
	aria-label={label}
	aria-hidden={label ? undefined : 'true'}
>
	<defs>
		<linearGradient id={gradientId} x1="0" x2="0" y1="0" y2="1">
			<stop offset="0%" stop-color={TONE_VAR[tone]} stop-opacity="0.22" />
			<stop offset="100%" stop-color={TONE_VAR[tone]} stop-opacity="0" />
		</linearGradient>
	</defs>
	{#if finitePoints(values) < 2}
		<line x1={PAD} x2={width - PAD} y1={height / 2} y2={height / 2} stroke="var(--color-border-strong)" stroke-width="1.5" stroke-dasharray="3 3" />
	{:else}
	{#each runs as run (run[0].i)}
		{#if run.length === 1}
			<circle cx={x(run[0].i)} cy={y(run[0].v)} r="1.75" fill={TONE_VAR[tone]} />
		{:else}
			{#if fill}<path d={area(run)} fill="url(#{gradientId})" />{/if}
			<path
				d={line(run)}
				fill="none"
				stroke={TONE_VAR[tone]}
				stroke-width="1.75"
				stroke-linejoin="round"
				stroke-linecap="round"
			/>
		{/if}
	{/each}
	{/if}
</svg>
