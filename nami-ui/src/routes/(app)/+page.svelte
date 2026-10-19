<script lang="ts">
	import Card from '#lib/components/ui/Card.svelte';
	import PageHeader from '#lib/components/ui/PageHeader.svelte';
	import type { PageProps } from './$types';
	let { data }: PageProps = $props();
</script>

<svelte:head>
	<title>Dashboard · nami</title>
</svelte:head>

<div class="space-y-6">
	<PageHeader title="Dashboard" subtitle="Signed in as {data.user.email}.">
		{#snippet actions()}
			<a href="/projects" class="btn btn-primary">Open projects</a>
		{/snippet}
	</PageHeader>

	<Card title="Your account">
		<dl class="grid grid-cols-1 gap-5 text-sm sm:grid-cols-3">
			<div>
				<dt class="text-xs text-muted">Email</dt>
				<dd class="mt-1 text-ink wrap-anywhere">{data.user.email}</dd>
			</div>
			<div>
				<dt class="text-xs text-muted">Global role</dt>
				<dd class="mt-1"><span class="badge badge-primary">{data.user.global_role}</span></dd>
			</div>
			<div>
				<dt class="text-xs text-muted">User ID</dt>
				<dd class="mt-1 font-mono text-xs break-all text-ink-2">{data.user.id}</dd>
			</div>
		</dl>
	</Card>
</div>
