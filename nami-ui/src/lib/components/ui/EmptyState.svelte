<script lang="ts">
	// The empty body of a list or table: an icon, a message, an optional action.
	import type { Snippet } from 'svelte';
	import Icon, { type IconName } from './Icon.svelte';

	let {
		icon = 'inbox',
		message,
		children
	}: { icon?: IconName; message: string; children?: Snippet } = $props();
</script>

<div class="flex flex-col items-center gap-3 px-6 py-12 text-center">
	<span class="flex h-11 w-11 items-center justify-center rounded-full bg-primary-soft text-primary">
		<Icon name={icon} size={20} />
	</span>
	<p class="max-w-sm text-sm text-muted">{message}</p>
	{#if children}{@render children()}{/if}
</div>
