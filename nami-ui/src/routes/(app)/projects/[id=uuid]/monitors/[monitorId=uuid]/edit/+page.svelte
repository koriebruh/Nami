<script lang="ts">
	import MonitorForm from '#lib/components/MonitorForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<svelte:head>
	<title>Edit {data.monitor.name} · Monitors</title>
</svelte:head>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h2 class="text-base font-semibold wrap-anywhere text-neutral-100">Edit {data.monitor.name}</h2>
	<div class="mt-4">
		<MonitorForm
			monitor={data.monitor}
			submitLabel="Save changes"
			message={form?.message ?? null}
			cancelHref="/projects/{data.project.id}/monitors/{data.monitor.id}"
		/>
	</div>
</div>
