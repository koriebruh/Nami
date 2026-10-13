<script lang="ts">
	import type { Status } from '#lib/uptime';

	let { status, paused = false }: { status: Status; paused?: boolean } = $props();

	// Status is never colour-alone: the dot always sits next to its label.
	const label = $derived(
		paused ? 'Paused' : status === 'up' ? 'Up' : status === 'down' ? 'Down' : 'Pending'
	);
	const dot = $derived(
		paused
			? 'bg-neutral-600'
			: status === 'up'
				? 'bg-teal-400'
				: status === 'down'
					? 'bg-red-400'
					: 'bg-neutral-500'
	);
	const text = $derived(
		paused || status === 'pending'
			? 'text-neutral-400'
			: status === 'up'
				? 'text-teal-300'
				: 'text-red-400'
	);
</script>

<span class="inline-flex items-center gap-2 text-sm {text}">
	<span class="h-2 w-2 shrink-0 rounded-full {dot}" aria-hidden="true"></span>
	{label}
</span>
