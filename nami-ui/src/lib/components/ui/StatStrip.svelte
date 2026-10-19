<script lang="ts">
	// The reference's KPI row: tiles side by side, wrapping on small screens.
	// `cols` fixes the desktop column count (2 on phones, up to 3 on tablets);
	// 'auto' fits as many tiles of at least `minWidth` as the row holds.
	import type { Snippet } from 'svelte';

	let {
		children,
		label,
		cols = 'auto',
		minWidth = '11rem'
	}: { children: Snippet; label?: string; cols?: 2 | 3 | 4 | 5 | 6 | 'auto'; minWidth?: string } = $props();

	const FIXED: Record<number, string> = {
		2: 'grid-cols-2',
		3: 'grid-cols-2 md:grid-cols-3',
		4: 'grid-cols-2 lg:grid-cols-4',
		5: 'grid-cols-2 md:grid-cols-3 xl:grid-cols-5',
		6: 'grid-cols-2 md:grid-cols-3 xl:grid-cols-6'
	};
</script>

<section
	aria-label={label}
	class="grid gap-3 sm:gap-4 {cols === 'auto' ? '' : FIXED[cols]}"
	style:grid-template-columns={cols === 'auto' ? `repeat(auto-fit, minmax(min(${minWidth}, 100%), 1fr))` : undefined}
>
	{@render children()}
</section>
