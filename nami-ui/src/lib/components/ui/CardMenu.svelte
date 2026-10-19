<script lang="ts">
	// The "…" menu of a card header: a native <details>, so it opens without
	// JavaScript and closes on a second click. Items are links or forms.
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';

	let { children, label = 'More actions' }: { children: Snippet; label?: string } = $props();
</script>

<details class="relative [&[open]>summary]:text-primary">
	<summary
		class="flex h-8 w-8 cursor-pointer list-none items-center justify-center rounded-control text-faint hover:bg-primary-soft hover:text-primary [&::-webkit-details-marker]:hidden"
		aria-label={label}
		title={label}
	>
		<Icon name="more" />
	</summary>
	<div
		class="absolute right-0 z-20 mt-1 min-w-44 rounded-card border border-border bg-surface py-1 text-sm shadow-pop [&_a]:block [&_a]:px-3 [&_a]:py-1.5 [&_a]:text-ink-2 [&_a:hover]:bg-primary-soft [&_a:hover]:text-primary"
	>
		{@render children()}
	</div>
</details>
