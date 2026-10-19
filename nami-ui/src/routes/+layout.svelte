<script lang="ts">
	// Inter, self-hosted: bundled by Vite from node_modules, no font CDN.
	import '@fontsource-variable/inter';
	import './layout.css';
	import favicon from '#lib/assets/favicon.svg';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
</svelte:head>

{@render children()}
