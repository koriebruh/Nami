<script lang="ts">
	import type { Snippet } from 'svelte';

	type BadgeTone = 'neutral' | 'primary' | 'success' | 'danger' | 'warning' | 'info';
	let {
		tone = 'neutral',
		dot = false,
		title,
		children
	}: { tone?: BadgeTone; dot?: boolean; title?: string; children: Snippet } = $props();

	const DOT: Record<BadgeTone, string> = {
		neutral: 'bg-status-paused',
		primary: 'bg-primary',
		success: 'bg-success',
		danger: 'bg-danger',
		warning: 'bg-warning',
		info: 'bg-info'
	};
</script>

<span class="badge badge-{tone}" {title}>
	{#if dot}<span class="h-1.5 w-1.5 shrink-0 rounded-full {DOT[tone]}" aria-hidden="true"></span>{/if}
	{@render children()}
</span>
