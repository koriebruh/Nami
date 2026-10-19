<script lang="ts">
	// Centered white card on the canvas, with the brand, for the signed-out pages.
	import type { Snippet } from 'svelte';
	import Brand from './Brand.svelte';

	let {
		title,
		subtitle = 'Monitor everything, self-hosted.',
		children,
		footer
	}: { title: string; subtitle?: string; children: Snippet; footer?: Snippet } = $props();
</script>

<svelte:head>
	<title>{title} · nami</title>
</svelte:head>

<div class="relative flex min-h-screen flex-col items-center justify-center overflow-hidden bg-canvas px-4 py-10">
	<!-- Decorative wash in the brand colours, like the reference's backdrop. -->
	<div
		aria-hidden="true"
		class="pointer-events-none absolute -top-40 -right-40 h-[28rem] w-[28rem] rounded-full bg-primary/10 blur-3xl"
	></div>
	<div
		aria-hidden="true"
		class="pointer-events-none absolute -bottom-40 -left-40 h-[24rem] w-[24rem] rounded-full bg-accent-cyan/10 blur-3xl"
	></div>

	<a href="/" class="relative mb-6" aria-label="nami home"><Brand /></a>
	<main class="card relative w-full max-w-[420px] p-7 sm:p-9">
		<div class="mb-6">
			<h1 class="display text-[1.75rem]">{title}</h1>
			<p class="mt-1 text-sm text-muted">{subtitle}</p>
		</div>
		{@render children()}
	</main>
	{#if footer}
		<p class="relative mt-6 text-center text-sm text-muted">{@render footer()}</p>
	{/if}
</div>
