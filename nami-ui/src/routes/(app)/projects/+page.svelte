<script lang="ts">
	import Card from '#lib/components/ui/Card.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import Icon from '#lib/components/ui/Icon.svelte';
	import PageHeader from '#lib/components/ui/PageHeader.svelte';
	import type { PageData, ActionData } from './$types';

	let { data, form }: { data: PageData; form: ActionData } = $props();
</script>

<svelte:head>
	<title>Projects · nami</title>
</svelte:head>

<div class="space-y-6">
	<PageHeader title="Projects" subtitle="Each project groups monitors, heartbeats, logs and incidents." />

	<div class="grid gap-6 lg:grid-cols-3">
		<div class="lg:col-span-2">
			<Card title="All projects" padded={false}>
				{#if data.projects.length === 0}
					<EmptyState icon="folder" message="No projects yet." />
				{:else}
					<ul class="divide-y divide-border">
						{#each data.projects as project (project.id)}
							<li>
								<a
									href="/projects/{project.id}"
									class="group flex items-center gap-4 px-5 py-4 transition-colors hover:bg-surface-2 sm:px-6"
								>
									<span
										class="flex h-10 w-10 shrink-0 items-center justify-center rounded-card bg-primary-soft text-sm font-semibold text-primary uppercase"
										aria-hidden="true">{project.name.slice(0, 2)}</span
									>
									<span class="min-w-0 flex-1">
										<span class="block truncate font-medium text-ink group-hover:text-primary">{project.name}</span>
										<span class="block truncate text-xs text-muted">{project.slug}</span>
									</span>
									<Icon name="chevronRight" size={16} class="text-faint group-hover:text-primary" />
								</a>
							</li>
						{/each}
					</ul>
				{/if}
			</Card>
		</div>

		<Card title="Create project">
			{#if form?.success === false}
				<div role="alert" class="alert alert-danger mb-4">
					{form.message}
				</div>
			{/if}

			<form method="POST" class="space-y-4">
				<div>
					<label for="name" class="label">Name</label>
					<input id="name" name="name" type="text" required class="input" />
				</div>
				<div>
					<label for="slug" class="label">Slug</label>
					<input id="slug" name="slug" type="text" required class="input" />
				</div>
				<button type="submit" class="btn btn-primary">
					<Icon name="plus" size={16} /> Create project
				</button>
			</form>
		</Card>
	</div>
</div>
