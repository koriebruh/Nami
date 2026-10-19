<script lang="ts">
	import { page } from '$app/state';
	import SegmentedTabs from '#lib/components/ui/SegmentedTabs.svelte';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	const base = $derived(`/projects/${data.project.id}/logs`);
	const sections = $derived(
		[
			{ href: base, label: 'Explorer', exact: true },
			{ href: `${base}/sources`, label: 'Sources', exact: false }
		].map((section) => ({
			href: section.href,
			label: section.label,
			active: section.exact ? page.url.pathname === section.href : page.url.pathname.startsWith(section.href)
		}))
	);
</script>

<div class="space-y-6">
	<SegmentedTabs label="Logs sections" items={sections} size="sm" />

	{@render children()}
</div>
