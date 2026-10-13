<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	const tabs = $derived([
		{ href: `/projects/${data.project.id}/monitors`, label: 'Monitors' },
		{ href: `/projects/${data.project.id}/settings`, label: 'Settings' }
	]);
</script>

<div class="space-y-6">
	<div>
		<a href="/projects" class="text-sm text-neutral-500 hover:text-neutral-300">← Projects</a>
		<h1 class="mt-2 text-lg font-semibold text-neutral-100">{data.project.name}</h1>
		<p class="text-sm text-neutral-500">{data.project.slug}</p>
	</div>

	<nav class="flex gap-1 border-b border-[#262626]" aria-label="Project sections">
		{#each tabs as tab (tab.href)}
			{@const active = page.url.pathname.startsWith(tab.href)}
			<a
				href={tab.href}
				aria-current={active ? 'page' : undefined}
				class="-mb-px border-b-2 px-3 py-2 text-sm transition-colors {active
					? 'border-teal-500 text-neutral-100'
					: 'border-transparent text-neutral-500 hover:text-neutral-300'}"
			>
				{tab.label}
			</a>
		{/each}
	</nav>

	{@render children()}
</div>
