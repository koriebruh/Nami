<script lang="ts">
	import MonitorForm from '#lib/components/MonitorForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h2 class="text-base font-semibold text-neutral-100">New monitor</h2>
	<div class="mt-4">
		<MonitorForm
			submitLabel="Create monitor"
			message={form?.message ?? null}
			cancelHref="/projects/{data.project.id}/monitors"
		/>
	</div>
</div>
