<script lang="ts">
	// A button, or a link styled as one when `href` is set. Pages that need
	// raw attributes on a form control can use the `.btn .btn-*` classes.
	import type { Snippet } from 'svelte';
	import type { HTMLButtonAttributes } from 'svelte/elements';

	type Variant = 'primary' | 'secondary' | 'ghost' | 'danger';
	let {
		variant = 'secondary',
		size = 'md',
		href,
		children,
		class: className = '',
		...rest
	}: HTMLButtonAttributes & {
		variant?: Variant;
		size?: 'sm' | 'md';
		href?: string;
		children: Snippet;
	} = $props();

	const classes = $derived(`btn btn-${variant} ${size === 'sm' ? 'btn-sm' : ''} ${className}`);
</script>

{#if href}
	<a {href} class={classes}>{@render children()}</a>
{:else}
	<button type="button" {...rest} class={classes}>{@render children()}</button>
{/if}
