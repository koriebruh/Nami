<script lang="ts">
	import Card from '#lib/components/ui/Card.svelte';
	import MaintenanceWindowForm from '#lib/components/MaintenanceWindowForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<svelte:head>
	<title>Edit {data.maintenanceWindow.title} · Maintenance</title>
</svelte:head>

<Card title="Edit {data.maintenanceWindow.title}">
	<MaintenanceWindowForm
		maintenanceWindow={data.maintenanceWindow}
		monitors={data.monitors}
		submitLabel="Save changes"
		message={form?.message ?? null}
		cancelHref="/projects/{data.project.id}/maintenance"
	/>
</Card>
