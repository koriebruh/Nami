<script lang="ts">
	// The "…" menu of a card header: a native <details>, so it opens without
	// JavaScript; with it, a click outside or Escape closes it. Put links and
	// buttons (form buttons too) inside with class="menu-item".
	import type { Snippet } from 'svelte';
	import { dismissible } from './dismiss';
	import Icon from './Icon.svelte';

	let { children, label = 'More actions' }: { children: Snippet; label?: string } = $props();
</script>

<details class="relative [&[open]>summary]:text-primary" use:dismissible>
	<summary
		class="flex h-8 w-8 cursor-pointer list-none items-center justify-center rounded-control text-muted hover:bg-primary-soft hover:text-primary [&::-webkit-details-marker]:hidden"
		aria-label={label}
		title={label}
	>
		<Icon name="more" />
	</summary>
	<div class="absolute right-0 z-20 mt-1 min-w-44 rounded-card border border-border bg-surface p-1 shadow-pop">
		{@render children()}
	</div>
</details>
