<script lang="ts">
	import Card from '#lib/components/ui/Card.svelte';
	import MonitorForm from '#lib/components/MonitorForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<svelte:head>
	<title>Edit {data.monitor.name} · Monitors</title>
</svelte:head>

<Card title="Edit {data.monitor.name}">
	<MonitorForm
		monitor={data.monitor}
		groups={data.groups}
		policies={data.policies}
		severities={data.severities}
		submitLabel="Save changes"
		message={form?.message ?? null}
		cancelHref="/projects/{data.project.id}/monitors/{data.monitor.id}"
	/>
</Card>
