<script lang="ts">
	import { enhance } from '$app/forms';
	import SeverityBadge from '#lib/components/SeverityBadge.svelte';
	import Card from '#lib/components/ui/Card.svelte';
	import { duration, eventLabel, sourceLabel, sourceLink, STATUS_BADGE } from '#lib/incidents/incidents';
	import { renderMarkdown } from '#lib/markdown';
	import { formatUtc } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	let now = $state(Date.now());
	$effect(() => {
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => clearInterval(clock);
	});

	const incident = $derived(data.incident);
	const canEdit = $derived(data.role === 'member' || data.role === 'admin');
	const sourceHref = $derived(sourceLink(data.project.id, incident));
	const list = $derived(`/projects/${data.project.id}/incidents`);
	const message = (action: string) => (form && 'action' in form && form.action === action ? form.message : null);
</script>

<svelte:head>
	<title>{incident.title} · Incidents · {data.project.name}</title>
</svelte:head>

{#snippet failure(action: string)}
	{@const text = message(action)}
	{#if text}<div role="alert" class="alert alert-danger">{text}</div>{/if}
{/snippet}

<div class="space-y-6">
	<a href={list} class="text-sm text-muted hover:text-primary">← Incidents</a>

	<div class="card space-y-4 p-5 sm:p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div class="min-w-0 space-y-2">
				<h2 class="text-lg font-light wrap-anywhere text-ink">{incident.title}</h2>
				<p class="flex flex-wrap items-center gap-2 text-sm text-muted">
					<SeverityBadge name={incident.severity} rank={incident.severity_rank} />
					<span class="badge {STATUS_BADGE[incident.status].class}">{STATUS_BADGE[incident.status].label}</span>
					<span>Started {formatUtc(incident.started_at)} · {duration(incident.started_at, incident.resolved_at, new Date(now))}</span>
				</p>
				<p class="text-sm wrap-anywhere text-ink-2">
					Source:
					{#if sourceHref}<a href={sourceHref} class="link">{sourceLabel(incident)}</a>{:else}{sourceLabel(incident)}{/if}
					{#if incident.created_by_email}· created by {incident.created_by_email}{/if}
				</p>
				{#if incident.acknowledged_at}
					<p class="text-xs wrap-anywhere text-muted">
						Acknowledged by {incident.acknowledged_by_email ?? 'someone'} at {formatUtc(incident.acknowledged_at)}
					</p>
				{/if}
				{#if incident.status === 'resolved' && incident.resolved_at}
					<p class="text-xs wrap-anywhere text-muted">
						Resolved{incident.resolved_by_email ? ` by ${incident.resolved_by_email}` : ''} at {formatUtc(incident.resolved_at)}
					</p>
				{/if}
			</div>
			{#if canEdit}
				<div class="flex flex-wrap items-center gap-2">
					{#if incident.status === 'open'}
						<form method="POST" action="?/acknowledge" use:enhance>
							<button type="submit" class="btn btn-primary btn-sm">Acknowledge</button>
						</form>
					{/if}
					{#if incident.status !== 'resolved'}
						<form method="POST" action="?/resolve" use:enhance>
							<button type="submit" class="btn btn-secondary btn-sm">Resolve</button>
						</form>
					{:else}
						<form method="POST" action="?/reopen" use:enhance>
							<button type="submit" class="btn btn-secondary btn-sm">Reopen</button>
						</form>
					{/if}
				</div>
			{/if}
		</div>
		{@render failure('acknowledge')}
		{@render failure('resolve')}
		{@render failure('reopen')}

		{#if incident.cause}
			<div>
				<h3 class="eyebrow mb-1.5">Cause</h3>
				<pre class="code max-h-64 overflow-auto break-all whitespace-pre-wrap">{incident.cause}</pre>
			</div>
		{/if}
		{#if incident.details}
			<!-- Job output from a failing heartbeat: text only. -->
			<details>
				<summary class="cursor-pointer text-xs text-primary">Job output</summary>
				<pre class="mt-2 max-h-64 overflow-auto rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all whitespace-pre-wrap text-ink-2">{incident.details}</pre>
			</details>
		{/if}

		{#if canEdit}
			<form method="POST" action="?/severity" use:enhance class="flex flex-wrap items-end gap-2">
				<div>
					<label for="incident-severity" class="label">Severity</label>
					<select id="incident-severity" name="severity_id" class="select">
						{#each data.severities as severity (severity.id)}
							<option value={severity.id} selected={severity.id === incident.severity_id}>{severity.name}</option>
						{/each}
					</select>
				</div>
				<button type="submit" class="btn btn-secondary">Change severity</button>
			</form>
			{@render failure('severity')}
		{/if}
	</div>

	<Card title="Timeline">
		{#if incident.events.length === 0}
			<p class="text-sm text-muted">No events yet.</p>
		{:else}
			<ol class="space-y-4 border-l border-border pl-4">
				{#each incident.events as event (event.id)}
					<li class="space-y-1">
						<p class="flex flex-wrap items-baseline gap-x-2 text-sm">
							<span class="font-medium text-ink">{eventLabel(event.kind)}</span>
							{#if event.actor_email}<span class="wrap-anywhere text-ink-2">{event.actor_email}</span>{/if}
							<time datetime={event.created_at} class="text-xs text-muted">{formatUtc(event.created_at)}</time>
						</p>
						{#if event.message}
							<!-- Text only; comments keep their line breaks. -->
							<p class="text-sm wrap-anywhere whitespace-pre-wrap text-ink-2">{event.message}</p>
						{/if}
					</li>
				{/each}
			</ol>
		{/if}
		{#if canEdit}
			<form method="POST" action="?/comment" use:enhance class="mt-6 space-y-2">
				<label for="incident-comment" class="label">Add a comment</label>
				<textarea id="incident-comment" name="body" rows="3" maxlength="10240" required class="input"></textarea>
				{@render failure('comment')}
				<button type="submit" class="btn btn-primary">Comment</button>
			</form>
		{/if}
	</Card>

	<Card title="Postmortem">
		{#if incident.postmortem}
			<!-- The only {@html} of the feature: renderMarkdown escapes everything
			     first and emits a fixed subset of tags (ruling R22). -->
			<div
				class="space-y-3 text-sm wrap-anywhere text-ink-2 [&_a]:text-primary [&_a]:underline [&_code]:rounded [&_code]:bg-surface-2 [&_code]:px-1 [&_code]:font-mono [&_code]:text-xs [&_h1]:text-xl [&_h1]:font-light [&_h1]:text-ink [&_h2]:text-lg [&_h2]:font-light [&_h2]:text-ink [&_h3]:font-medium [&_h3]:text-ink [&_h4]:font-medium [&_h5]:font-medium [&_h6]:font-medium [&_ol]:list-decimal [&_ol]:pl-5 [&_pre]:overflow-auto [&_pre]:rounded-control [&_pre]:border [&_pre]:border-border [&_pre]:bg-surface-2 [&_pre]:px-3 [&_pre]:py-2 [&_pre_code]:bg-transparent [&_pre_code]:px-0 [&_ul]:list-disc [&_ul]:pl-5"
			>
				{@html renderMarkdown(incident.postmortem)}
			</div>
		{:else}
			<p class="text-sm text-muted">No postmortem yet.</p>
		{/if}
		{#if canEdit}
			<details class="mt-4" open={message('postmortem') !== null}>
				<summary class="summary">{incident.postmortem ? 'Edit postmortem' : 'Write a postmortem'}</summary>
				<form method="POST" action="?/postmortem" use:enhance class="mt-3 space-y-2">
					<label for="incident-postmortem" class="label">Postmortem (markdown)</label>
					<textarea
						id="incident-postmortem"
						name="postmortem"
						rows="12"
						maxlength="102400"
						defaultValue={incident.postmortem ?? ''}
						class="input font-mono text-xs"
					></textarea>
					<p class="hint">Headings, lists, **bold**, *italic*, `code`, fenced code and http(s) links. Leave empty to remove it.</p>
					{@render failure('postmortem')}
					<button type="submit" class="btn btn-primary">Save postmortem</button>
				</form>
			</details>
		{/if}
	</Card>
</div>
