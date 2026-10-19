<script lang="ts">
	import MaintenanceWindowForm from '#lib/components/MaintenanceWindowForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<svelte:head>
	<title>Edit {data.maintenanceWindow.title} · Maintenance</title>
</svelte:head>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h2 class="text-base font-semibold wrap-anywhere text-neutral-100">Edit {data.maintenanceWindow.title}</h2>
	<div class="mt-4">
		<MaintenanceWindowForm
			maintenanceWindow={data.maintenanceWindow}
			monitors={data.monitors}
			submitLabel="Save changes"
			message={form?.message ?? null}
			cancelHref="/projects/{data.project.id}/maintenance"
		/>
	</div>
</div>
