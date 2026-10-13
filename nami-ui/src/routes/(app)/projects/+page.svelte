<script lang="ts">
	import type { PageData, ActionData } from './$types';

	let { data, form }: { data: PageData; form: ActionData } = $props();
</script>

<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h1 class="text-lg font-semibold text-neutral-100">Projects</h1>

		<div class="mt-4 space-y-2">
			{#if data.projects.length === 0}
				<p class="text-sm text-neutral-500">No projects yet.</p>
			{:else}
				{#each data.projects as project (project.id)}
					<a
						href="/projects/{project.id}"
						class="flex items-center justify-between rounded-md border border-[#262626] px-4 py-3 text-sm hover:border-teal-500/30"
					>
						<span class="font-medium text-neutral-100">{project.name}</span>
						<span class="text-neutral-500">{project.slug}</span>
					</a>
				{/each}
			{/if}
		</div>
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-lg font-semibold text-neutral-100">Create project</h2>

		{#if form?.success === false}
			<div class="mt-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				{form.message}
			</div>
		{/if}

		<form method="POST" class="mt-4 space-y-4">
			<div>
				<label for="name" class="mb-1.5 block text-sm font-medium text-neutral-300">Name</label>
				<input
					id="name"
					name="name"
					type="text"
					required
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				/>
			</div>
			<div>
				<label for="slug" class="mb-1.5 block text-sm font-medium text-neutral-300">Slug</label>
				<input
					id="slug"
					name="slug"
					type="text"
					required
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				/>
			</div>
			<button
				type="submit"
				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
			>
				Create project
			</button>
		</form>
	</div>
</div>
