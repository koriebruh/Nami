<script lang="ts">
	// Export the shown window as CSV or NDJSON. Plain download links (the BFF
	// relays the engine's stream); `warning` is asked first when the window
	// holds more rows than an export carries (R-P2-16).
	import { dismissible } from '#lib/components/ui';

	let { csvHref, ndjsonHref, warning }: { csvHref: string; ndjsonHref: string; warning: string | null } = $props();

	function ask(event: MouseEvent) {
		if (warning && !confirm(warning)) event.preventDefault();
	}
</script>

<details class="relative" use:dismissible>
	<summary class="btn btn-secondary cursor-pointer list-none [&::-webkit-details-marker]:hidden">Export</summary>
	<div class="absolute right-0 z-20 mt-1 min-w-44 rounded-card border border-border bg-surface p-1 shadow-pop">
		<a href={csvHref} download data-sveltekit-reload onclick={ask} class="menu-item">CSV</a>
		<a href={ndjsonHref} download data-sveltekit-reload onclick={ask} class="menu-item">NDJSON</a>
		{#if warning}<p class="px-3 py-1.5 text-xs text-warning-ink">{warning}</p>{/if}
	</div>
</details>
