<script lang="ts">
	// Large light page title (the reference's "Dashboard"), an optional
	// subtitle and range pill on its right, and actions at the far right.
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';

	let {
		title,
		subtitle,
		range,
		eyebrow,
		actions,
		children
	}: {
		title: string;
		subtitle?: string;
		/** A date-range / refresh pill, e.g. "Last 24 hours". */
		range?: string;
		/** A breadcrumb or context line above the title. */
		eyebrow?: Snippet;
		actions?: Snippet;
		/** Extra content under the title row (badges, meta). */
		children?: Snippet;
	} = $props();
</script>

<header class="flex flex-wrap items-end justify-between gap-4">
	<div class="min-w-0">
		{#if eyebrow}<div class="mb-1 text-sm text-muted">{@render eyebrow()}</div>{/if}
		<div class="flex flex-wrap items-center gap-x-5 gap-y-2">
			<h1 class="display wrap-anywhere">{title}</h1>
			{#if range}
				<span
					class="inline-flex items-center gap-2 rounded-control border border-border-strong bg-surface px-3 py-1.5 text-xs text-ink-2"
				>
					<Icon name="calendar" size={14} class="text-muted" />
					{range}
				</span>
			{/if}
		</div>
		{#if subtitle}<p class="mt-1 text-sm text-muted wrap-anywhere">{subtitle}</p>{/if}
		{#if children}<div class="mt-2">{@render children()}</div>{/if}
	</div>
	{#if actions}<div class="flex flex-wrap items-center gap-2">{@render actions()}</div>{/if}
</header>
