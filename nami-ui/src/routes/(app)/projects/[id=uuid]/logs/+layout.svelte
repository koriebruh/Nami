<script lang="ts">
	import { page } from '$app/state';
	import SegmentedTabs from '#lib/components/ui/SegmentedTabs.svelte';
	import { explorerParams, parseExplorerState } from '#lib/logs/logs';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	const base = $derived(`/projects/${data.project.id}/logs`);
	const sections = $derived(
		[
			{ href: base, label: 'Explorer', exact: true },
			{ href: `${base}/sources`, label: 'Sources', exact: false },
			{ href: `${base}/alerts`, label: 'Alerts', exact: false }
		].map((section) => ({
			href: section.href,
			label: section.label,
			active: section.exact ? page.url.pathname === section.href : page.url.pathname.startsWith(section.href)
		}))
	);
	// On the explorer: an alert that counts what the current search shows (the
	// explorer keeps its whole state in the URL, through the same rules).
	const onExplorer = $derived(page.url.pathname === base);
	const alertHref = $derived(`${base}/alerts/new?${explorerParams(parseExplorerState(new URLSearchParams(page.url.search)))}`);
</script>

<div class="space-y-6">
	<div class="flex flex-wrap items-center justify-between gap-3">
		<SegmentedTabs label="Logs sections" items={sections} size="sm" />
		{#if onExplorer}
			<a href={alertHref} class="btn btn-secondary btn-sm">Create alert from this search</a>
		{/if}
	</div>

	{@render children()}
</div>
