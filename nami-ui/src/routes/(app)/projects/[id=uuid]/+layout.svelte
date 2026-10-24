<script lang="ts">
	import { page } from '$app/state';
	import PageHeader from '#lib/components/ui/PageHeader.svelte';
	import SegmentedTabs from '#lib/components/ui/SegmentedTabs.svelte';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	const tabs = $derived(
		[
			{ href: `/projects/${data.project.id}/monitors`, label: 'Monitors' },
			{ href: `/projects/${data.project.id}/heartbeats`, label: 'Heartbeats' },
			{ href: `/projects/${data.project.id}/incidents`, label: 'Incidents' },
			{ href: `/projects/${data.project.id}/oncall`, label: 'On-call' },
			{ href: `/projects/${data.project.id}/logs`, label: 'Logs' },
			{ href: `/projects/${data.project.id}/maintenance`, label: 'Maintenance' },
			{ href: `/projects/${data.project.id}/settings`, label: 'Settings' }
		].map((tab) => ({ ...tab, active: page.url.pathname.startsWith(tab.href) }))
	);
</script>

<div class="space-y-6">
	<PageHeader title={data.project.name} subtitle={data.project.slug}>
		{#snippet eyebrow()}
			<a href="/projects" class="hover:text-primary">← Projects</a>
		{/snippet}
	</PageHeader>

	<div class="border-b border-border pb-3">
		<SegmentedTabs label="Project sections" items={tabs} />
	</div>

	{@render children()}
</div>
