<script lang="ts">
	import AuthCard from '#lib/components/ui/AuthCard.svelte';
	import { isUuid } from '#lib/core/uuid';
	import { formatUtc } from '#lib/uptime/uptime';
	import type { ActionData, PageData } from './$types';

	let { data, form }: { data: PageData; form: ActionData } = $props();

	const inspected = $derived(data.inspected);
	const verb = $derived(inspected?.action === 'resolve' ? 'Resolve' : 'Acknowledge');
	// Built only from ids that are UUIDs: the page takes no redirect target.
	const incidentHref = $derived(
		inspected && isUuid(inspected.incident.project_id) && isUuid(inspected.incident.id)
			? `/projects/${inspected.incident.project_id}/incidents/${inspected.incident.id}`
			: null
	);
	// Engine messages are lowercase phrases; the page's own end in a full stop.
	const sentence = (text: string) => (/[.!?]$/.test(text) ? text : `${text}.`);
</script>

<AuthCard title={inspected ? `${verb} incident` : 'Incident link'} subtitle="From a Nami incident email.">
	{#if form && 'done' in form}
		<div role="status" class="alert alert-success">The incident is {form.status} now.</div>
	{:else if data.problem}
		<div role="alert" class="alert alert-danger first-letter:uppercase">{sentence(data.problem)}</div>
	{:else if inspected && data.token}
		<dl class="mb-6 grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
			<dt class="text-muted">Project</dt>
			<dd class="break-words text-ink">{inspected.incident.project_name}</dd>
			<dt class="text-muted">Incident</dt>
			<dd class="break-words font-medium text-ink">{inspected.incident.title}</dd>
			<dt class="text-muted">Severity</dt>
			<dd><span class="badge badge-warning">{inspected.incident.severity}</span></dd>
			<dt class="text-muted">Status</dt>
			<dd>
				<span class="badge {inspected.incident.status === 'resolved' ? 'badge-neutral' : 'badge-danger'}"
					>{inspected.incident.status}</span
				>
			</dd>
			<dt class="text-muted">Started</dt>
			<dd class="text-ink">{formatUtc(inspected.incident.started_at)}</dd>
		</dl>
		{#if form?.message}
			<div role="alert" class="alert alert-danger mb-4 first-letter:uppercase">{sentence(form.message)}</div>
		{/if}
		<!-- The token stays in the action URL so the page can load again after the POST; the action reads it from the body. -->
		<form method="POST" action="?/confirm&token={encodeURIComponent(data.token)}" class="flex flex-col gap-3">
			<input type="hidden" name="token" value={data.token} />
			<button type="submit" class="btn btn-primary w-full py-2.5">{verb} incident</button>
			<p class="text-center text-xs text-muted">This acts as {inspected.user_email}.</p>
		</form>
	{/if}

	{#snippet footer()}
		{#if incidentHref}
			<a href={incidentHref} class="link">Open the incident in Nami</a>
		{:else}
			<a href="/" class="link">Go to Nami</a>
		{/if}
	{/snippet}
</AuthCard>
