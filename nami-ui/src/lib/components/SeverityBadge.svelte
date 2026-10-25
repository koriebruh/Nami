<script lang="ts">
	import Badge from '#lib/components/ui/Badge.svelte';

	// Rank 1 is the most severe. The name is always shown next to the dot, so
	// severity is never told by colour alone.
	let { name, rank }: { name: string | null; rank: number | null } = $props();

	const tone = $derived(rank === 1 ? 'danger' : rank === 2 ? 'warning' : 'neutral');
</script>

{#if name}
	<Badge {tone} dot title="Severity">{name}</Badge>
{:else}
	<span class="text-xs text-muted">No severity</span>
{/if}
