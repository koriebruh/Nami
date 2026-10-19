<script lang="ts">
	// The reference's KPI row: tiles side by side, wrapping on small screens.
	import type { Snippet } from 'svelte';

	let { children, label }: { children: Snippet; label?: string } = $props();
</script>

<section aria-label={label} class="grid grid-cols-2 gap-3 sm:gap-4 md:grid-cols-3 xl:grid-cols-5">
	{@render children()}
</section>
