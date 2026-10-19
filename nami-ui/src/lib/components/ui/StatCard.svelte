<script lang="ts">
	// KPI tile: small label, big value (+ optional unit), an optional coloured
	// delta with an arrow, an optional hint and an optional sparkline. Colour
	// never carries meaning alone: the delta keeps its sign and arrow, the hint
	// its words. `loading` shows a skeleton and marks the tile busy.
	import Icon from './Icon.svelte';
	import Sparkline from './Sparkline.svelte';
	import { finitePoints, TONE_TEXT, type Tone } from './tones';

	let {
		label,
		value,
		unit,
		delta,
		direction,
		deltaTone,
		hint,
		spark,
		sparkTone = 'primary',
		sparkLabel,
		valueTone,
		href,
		loading = false
	}: {
		label: string;
		value: string;
		/** Shown smaller after the value, e.g. "ms" or "%". */
		unit?: string;
		/** e.g. "+0.25"; shown next to the label. */
		delta?: string;
		direction?: 'up' | 'down';
		deltaTone?: Tone;
		hint?: string;
		spark?: (number | null)[];
		sparkTone?: Tone;
		sparkLabel?: string;
		valueTone?: Tone;
		href?: string;
		loading?: boolean;
	} = $props();

	const tone = $derived(deltaTone ?? (direction === 'down' ? 'danger' : 'success'));
	// A trend needs two points; Sparkline draws a flat baseline otherwise, but
	// a tile with no trend at all simply shows none.
	const showSpark = $derived(spark !== undefined && finitePoints(spark) >= 2);
</script>

{#snippet content()}
	<div class="flex items-start justify-between gap-2">
		<p class="text-xs text-muted">{label}</p>
		{#if delta && !loading}
			<span class="inline-flex items-center gap-0.5 text-xs font-medium {TONE_TEXT[tone]}">
				{#if direction}<Icon name={direction === 'up' ? 'arrowUp' : 'arrowDown'} size={12} strokeWidth={2.5} />{/if}
				{delta}
			</span>
		{/if}
	</div>
	{#if loading}
		<div class="mt-2 h-7 w-24 animate-pulse rounded-control bg-border motion-reduce:animate-none" aria-hidden="true"></div>
		<p class="sr-only">Loading {label}</p>
	{:else}
		<div class="mt-1 flex items-end justify-between gap-3">
			<p class="text-2xl font-normal tracking-tight tabular-nums {valueTone ? TONE_TEXT[valueTone] : 'text-ink'}">
				{value}{#if unit}<span class="ml-1 text-sm font-normal text-muted">{unit}</span>{/if}
			</p>
			{#if showSpark && spark}
				<Sparkline values={spark} tone={sparkTone} label={sparkLabel} />
			{/if}
		</div>
		{#if hint}<p class="mt-1 text-xs text-muted">{hint}</p>{/if}
	{/if}
{/snippet}

{#if href}
	<a {href} aria-busy={loading || undefined} class="card block p-4 transition-shadow hover:shadow-pop">{@render content()}</a>
{:else}
	<div aria-busy={loading || undefined} class="card p-4">{@render content()}</div>
{/if}
