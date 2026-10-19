<script lang="ts">
	// KPI tile: small label, big value, an optional coloured delta with an
	// arrow, an optional hint and an optional sparkline. Colour never carries
	// meaning alone: the delta keeps its sign and arrow, the hint its words.
	import Icon from './Icon.svelte';
	import Sparkline from './Sparkline.svelte';
	import { TONE_TEXT, type Tone } from './tones';

	let {
		label,
		value,
		delta,
		direction,
		deltaTone,
		hint,
		spark,
		sparkTone = 'primary',
		sparkLabel,
		valueTone,
		href
	}: {
		label: string;
		value: string;
		/** e.g. "+0.25" or "3 down"; shown next to the label. */
		delta?: string;
		direction?: 'up' | 'down';
		deltaTone?: Tone;
		hint?: string;
		spark?: (number | null)[];
		sparkTone?: Tone;
		sparkLabel?: string;
		valueTone?: Tone;
		href?: string;
	} = $props();

	const tone = $derived(deltaTone ?? (direction === 'down' ? 'danger' : 'success'));
</script>

{#snippet content()}
	<div class="flex items-start justify-between gap-2">
		<p class="text-xs text-muted">{label}</p>
		{#if delta}
			<span class="inline-flex items-center gap-0.5 text-xs font-medium {TONE_TEXT[tone]}">
				{#if direction}<Icon name={direction === 'up' ? 'arrowUp' : 'arrowDown'} size={12} strokeWidth={2.5} />{/if}
				{delta}
			</span>
		{/if}
	</div>
	<div class="mt-1 flex items-end justify-between gap-3">
		<p class="text-2xl font-normal tracking-tight tabular-nums {valueTone ? TONE_TEXT[valueTone] : 'text-ink'}">
			{value}
		</p>
		{#if spark && spark.length > 1}
			<Sparkline values={spark} tone={sparkTone} label={sparkLabel} />
		{/if}
	</div>
	{#if hint}<p class="mt-1 text-xs text-muted">{hint}</p>{/if}
{/snippet}

{#if href}
	<a {href} class="card block p-4 transition-shadow hover:shadow-pop">{@render content()}</a>
{:else}
	<div class="card p-4">{@render content()}</div>
{/if}
