<script lang="ts">
	// The nami wordmark: a wave glyph in primary, lowercase name.
	let { compact = false, class: className = '' }: { compact?: boolean; class?: string } = $props();
</script>

<span class="inline-flex items-center gap-2 {className}">
	<svg width="28" height="28" viewBox="0 0 32 32" aria-hidden="true" class="shrink-0">
		<rect width="32" height="32" rx="8" fill="var(--color-primary)" />
		<path
			d="M6 19c2.5 0 2.5-3 5-3s2.5 3 5 3 2.5-3 5-3 2.5 3 5 3"
			fill="none"
			stroke="#fff"
			stroke-width="2.2"
			stroke-linecap="round"
		/>
		<path
			d="M6 13c2.5 0 2.5-3 5-3s2.5 3 5 3 2.5-3 5-3 2.5 3 5 3"
			fill="none"
			stroke="var(--color-accent-cyan)"
			stroke-width="2.2"
			stroke-linecap="round"
		/>
	</svg>
	{#if !compact}<span class="text-lg font-semibold tracking-tight text-ink">nami</span>{/if}
</span>
