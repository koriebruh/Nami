<script lang="ts">
	// White card with an optional header: title (+ subtitle) on the left,
	// segmented tabs in the middle, actions and a menu on the right.
	import type { Snippet } from 'svelte';

	let {
		title,
		subtitle,
		titleTag = 'h2',
		tabs,
		actions,
		menu,
		children,
		padded = true,
		class: className = '',
		id
	}: {
		title?: string;
		subtitle?: string;
		titleTag?: 'h1' | 'h2' | 'h3';
		tabs?: Snippet;
		actions?: Snippet;
		menu?: Snippet;
		children: Snippet;
		/** false when the body is a full-bleed table or list. */
		padded?: boolean;
		class?: string;
		id?: string;
	} = $props();

	const hasHeader = $derived(Boolean(title || tabs || actions || menu));
</script>

<section class="card {className}" {id}>
	{#if hasHeader}
		<div class="card-header">
			<div class="flex min-w-0 flex-wrap items-center gap-x-6 gap-y-2">
				{#if title}
					<div class="min-w-0">
						<svelte:element this={titleTag} class="card-title wrap-anywhere">{title}</svelte:element>
						{#if subtitle}<p class="text-xs text-muted">{subtitle}</p>{/if}
					</div>
				{/if}
				{#if tabs}{@render tabs()}{/if}
			</div>
			{#if actions || menu}
				<div class="flex flex-wrap items-center gap-2">
					{#if actions}{@render actions()}{/if}
					{#if menu}{@render menu()}{/if}
				</div>
			{/if}
		</div>
	{/if}
	<div class={padded ? 'card-body' : ''}>
		{@render children()}
	</div>
</section>
