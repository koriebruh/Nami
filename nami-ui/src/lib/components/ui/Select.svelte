<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLSelectAttributes } from 'svelte/elements';

	let {
		id,
		label,
		hint,
		children,
		class: className = '',
		...rest
	}: HTMLSelectAttributes & { id: string; label?: string; hint?: string; children: Snippet } = $props();
</script>

<div class={className}>
	{#if label}<label for={id} class="label">{label}</label>{/if}
	<select {id} {...rest} class="select" aria-describedby={hint ? `${id}-hint` : undefined}>
		{@render children()}
	</select>
	{#if hint}<p id="{id}-hint" class="hint">{hint}</p>{/if}
</div>
