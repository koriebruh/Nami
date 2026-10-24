<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import LogAlertForm from '#lib/components/logs/LogAlertForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const base = $derived(`/projects/${data.project.id}/logs/alerts`);

	let busy = $state(false);
	const submit: SubmitFunction = () => {
		busy = true;
		return async ({ update }) => {
			try {
				await update({ reset: false });
			} finally {
				busy = false;
			}
		};
	};
</script>

<svelte:head>
	<title>New alert · Logs · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<a href={base} class="text-sm text-muted hover:text-primary">← Alerts</a>

	<div class="card space-y-4 p-5 sm:p-6">
		<h2 class="text-lg font-light text-ink">New log alert</h2>
		{#if form?.message}
			<div role="alert" class="alert alert-danger">{form.message}</div>
		{/if}
		<form method="POST" action="?/create" use:enhance={submit}>
			<LogAlertForm initial={data.initial} sources={data.sources} previewUrl="{base}/preview" submitLabel="Create alert" {busy} />
		</form>
	</div>
</div>
