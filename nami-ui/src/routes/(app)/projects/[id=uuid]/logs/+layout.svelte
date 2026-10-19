<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	const base = $derived(`/projects/${data.project.id}/logs`);
	const sections = $derived([{ href: `${base}/sources`, label: 'Sources' }]);
</script>

<div class="space-y-6">
	<nav class="flex gap-2" aria-label="Logs sections">
		{#each sections as section (section.href)}
			{@const active = page.url.pathname.startsWith(section.href)}
			<a
				href={section.href}
				aria-current={active ? 'page' : undefined}
				class="rounded-md px-3 py-1.5 text-sm transition-colors {active
					? 'bg-[#1f1f1f] text-neutral-100'
					: 'text-neutral-500 hover:text-neutral-300'}"
			>
				{section.label}
			</a>
		{/each}
	</nav>

	{@render children()}
</div>
