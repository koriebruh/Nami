<script lang="ts">
	// A labelled text input. Initial text goes through `defaultValue` (never
	// value=) so a re-render cannot wipe what the user typed.
	import type { HTMLInputAttributes } from 'svelte/elements';

	let {
		id,
		label,
		hint,
		class: className = '',
		...rest
	}: HTMLInputAttributes & { id: string; label?: string; hint?: string } = $props();
</script>

<div class={className}>
	{#if label}<label for={id} class="label">{label}</label>{/if}
	<input {id} {...rest} class="input" aria-describedby={hint ? `${id}-hint` : undefined} />
	{#if hint}<p id="{id}-hint" class="hint">{hint}</p>{/if}
</div>
