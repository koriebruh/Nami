<script lang="ts">
	import ChannelList from '#lib/components/ChannelList.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<svelte:head>
	<title>Notification channels · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<div>
		<a href="/projects/{data.project.id}/settings" class="link text-sm">← Settings</a>
		<h2 class="mt-2 text-lg font-light text-ink">Notification channels</h2>
		<p class="mt-1 text-sm text-muted">Project-wide channels that escalation policy steps can target.</p>
		{#if data.role !== 'admin'}
			<p class="mt-2 text-sm text-muted">Only project admins can change project channels.</p>
		{/if}
	</div>
	<ChannelList channels={data.channels} {form} scope="project" canEdit={data.role === 'admin'} />
</div>
