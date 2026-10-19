<script lang="ts">
	import type { Status } from '#lib/uptime';

	let { status, paused = false }: { status: Status; paused?: boolean } = $props();

	// Status is never colour-alone: the dot always sits next to its label.
	// Tones come from the design tokens: up = success, down = danger,
	// pending = warning, paused = neutral (all AA text on their tint).
	const label = $derived(
		paused ? 'Paused' : status === 'up' ? 'Up' : status === 'down' ? 'Down' : 'Pending'
	);
	const tone = $derived(
		paused
			? 'bg-paused-soft text-paused-ink'
			: status === 'up'
				? 'bg-success-soft text-success-ink'
				: status === 'down'
					? 'bg-danger-soft text-danger-ink'
					: 'bg-warning-soft text-warning-ink'
	);
	const dot = $derived(
		paused
			? 'bg-status-paused'
			: status === 'up'
				? 'bg-status-up'
				: status === 'down'
					? 'bg-status-down'
					: 'bg-status-pending'
	);
</script>

<span class="inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium whitespace-nowrap {tone}">
	<span class="h-1.5 w-1.5 shrink-0 rounded-full {dot}" aria-hidden="true"></span>
	{label}
</span>
