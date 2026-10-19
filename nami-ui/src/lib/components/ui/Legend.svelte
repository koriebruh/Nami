<script lang="ts">
	// Small hollow dots + labels, as in the reference's chart legends.
	import { TONE_VAR, type Tone } from './tones';

	let { items, class: className = '' }: { items: { label: string; tone: Tone }[]; class?: string } = $props();
</script>

<ul class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted {className}">
	{#each items as item (item.label)}
		<li class="inline-flex items-center gap-1.5">
			<span
				class="inline-block h-2.5 w-2.5 rounded-full border-2 bg-surface"
				style:border-color={TONE_VAR[item.tone]}
				aria-hidden="true"
			></span>
			{item.label}
		</li>
	{/each}
</ul>
