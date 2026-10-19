<script lang="ts">
	import Card from '#lib/components/ui/Card.svelte';
	import MonitorForm from '#lib/components/MonitorForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<svelte:head>
	<title>New monitor · {data.project.name}</title>
</svelte:head>

<Card title="New monitor">
	<MonitorForm
		groups={data.groups}
		submitLabel="Create monitor"
		message={form?.message ?? null}
		cancelHref="/projects/{data.project.id}/monitors"
	/>
</Card>
