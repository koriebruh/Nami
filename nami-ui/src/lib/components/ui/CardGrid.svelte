<script lang="ts">
	// The reference's two-column rows: a wide card and a narrow card side by
	// side on desktop (2:1 by default), stacked on smaller screens.
	import type { Snippet } from 'svelte';

	let {
		wide,
		narrow,
		ratio = '2:1',
		reverse = false
	}: { wide: Snippet; narrow: Snippet; ratio?: '2:1' | '3:2' | '1:1'; reverse?: boolean } = $props();

	const COLS = { '2:1': 'lg:grid-cols-3', '3:2': 'lg:grid-cols-5', '1:1': 'lg:grid-cols-2' } as const;
	const WIDE = { '2:1': 'lg:col-span-2', '3:2': 'lg:col-span-3', '1:1': '' } as const;
	const NARROW = { '2:1': '', '3:2': 'lg:col-span-2', '1:1': '' } as const;
</script>

<div class="grid gap-6 {COLS[ratio]}">
	<div class="min-w-0 {WIDE[ratio]} {reverse ? 'lg:order-2' : ''}">{@render wide()}</div>
	<div class="min-w-0 {NARROW[ratio]}">{@render narrow()}</div>
</div>
