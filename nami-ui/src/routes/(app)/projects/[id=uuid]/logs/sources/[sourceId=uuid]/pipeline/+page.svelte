<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import PipelineEditor from '#lib/components/logs/PipelineEditor.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const sourceHref = $derived(`/projects/${data.project.id}/logs/sources/${data.source.id}`);

	let valid = $state(false);
	let busy = $state(false);
	const submit: SubmitFunction = (submission) => {
		// Never post rules the editor has not validated.
		if (!valid || busy) {
			submission.cancel();
			return;
		}
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
	<title>Pipeline · {data.source.name} · Logs</title>
</svelte:head>

<div class="space-y-6">
	<a href={sourceHref} class="text-sm text-muted hover:text-primary">← {data.source.name}</a>

	<div class="card space-y-4 p-5 sm:p-6">
		<div class="space-y-1">
			<h2 class="text-lg font-light wrap-anywhere text-ink">Pipeline · {data.source.name}</h2>
			<p class="text-sm text-muted">
				Rules run top to bottom after parsing and size caps, before patterns and storage. Edits reach ingestion on the
				next request. Redacted values are never stored. A redact rule covers what is there at its position, so put it
				after any rename that moves a secret.
			</p>
		</div>

		{#if form?.saved}
			<div role="status" class="alert alert-success">Saved. The next request uses these rules.</div>
		{/if}
		{#if form?.message}
			<div role="alert" class="alert alert-danger">
				{form.message.replace(/rules\[(\d+)\]/g, (_, i) => `Rule ${Number(i) + 1}`)}
			</div>
		{/if}

		<!-- Keyed, so that opening another source starts from its own rules. -->
		{#key data.source.id}
			<form method="POST" action="?/save" use:enhance={submit} class="space-y-4">
				<PipelineEditor rules={data.rules} previewUrl="{sourceHref}/pipeline/test" bind:valid />
				<div class="border-t border-border pt-4">
					<button type="submit" class="btn btn-primary" disabled={!valid || busy}>Save</button>
				</div>
			</form>
		{/key}
	</div>
</div>
