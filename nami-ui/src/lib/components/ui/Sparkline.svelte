<script lang="ts">
	// A dependency-free SVG sparkline. Null values are gaps; the line breaks
	// there instead of interpolating. Decorative unless `label` is given.
	import { TONE_VAR, type Tone } from './tones';

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
	const gradientId = `spark-${Math.random().toString(36).slice(2, 9)}`;

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
</svg>
