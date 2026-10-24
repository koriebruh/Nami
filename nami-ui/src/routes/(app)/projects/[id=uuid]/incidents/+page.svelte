<script lang="ts">
	import { enhance } from '$app/forms';
	import { refreshAll } from '$app/navigation';
	import SeverityBadge from '#lib/components/SeverityBadge.svelte';
	import Card from '#lib/components/ui/Card.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import SegmentedTabs from '#lib/components/ui/SegmentedTabs.svelte';
	import { duration, SOURCE_LABELS, SOURCES, sourceLabel, sourceLink, STATUS_BADGE, type ListTab } from '#lib/incidents/incidents';
	import { formatUtc } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});

	const canEdit = $derived(data.role === 'member' || data.role === 'admin');

	/** The list's query string for a tab, source and cursor. */
	function listHref(tab: ListTab, source: string | null, cursor: string | null = null): string {
		const params = new URLSearchParams({ tab });
		if (source) params.set('source', source);
		if (cursor) params.set('cursor', cursor);
		return `?${params}`;
	}

	const TABS: { tab: ListTab; label: string }[] = [
		{ tab: 'unresolved', label: 'Unresolved' },
		{ tab: 'acknowledged', label: 'Acknowledged' },
		{ tab: 'resolved', label: 'Resolved' },
		{ tab: 'all', label: 'All' }
	];
	const tabItems = $derived(
		TABS.map(({ tab, label }) => ({ href: listHref(tab, data.query.source), label, active: data.query.tab === tab }))
	);

	const EMPTY: Record<ListTab, string> = {
		unresolved: 'No unresolved incidents — everything is up.',
		acknowledged: 'No acknowledged incidents.',
		resolved: 'No resolved incidents yet.',
		all: 'No incidents yet.'
	};
</script>

<svelte:head>
	<title>Incidents · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	{#if canEdit}
		<details class="card" open={form?.action === 'create'}>
			<summary class="summary px-5 py-4 sm:px-6">New incident</summary>
			<form method="POST" action="?/create" use:enhance class="grid gap-4 border-t border-border px-5 py-5 sm:grid-cols-2 sm:px-6">
				{#if form?.action === 'create' && form.message}
					<div role="alert" class="alert alert-danger sm:col-span-2">{form.message}</div>
				{/if}
				<div class="sm:col-span-2">
					<label for="incident-title" class="label">Title</label>
					<input id="incident-title" name="title" required maxlength="255" class="input" />
				</div>
				<div class="sm:col-span-2">
					<label for="incident-description" class="label">Description</label>
					<textarea id="incident-description" name="description" rows="3" maxlength="10240" class="input"></textarea>
				</div>
				<div>
					<label for="incident-severity" class="label">Severity</label>
					<select id="incident-severity" name="severity_id" class="select">
						<option value="">Project default</option>
						{#each data.severities as severity (severity.id)}
							<option value={severity.id}>{severity.name}{severity.is_default ? ' (default)' : ''}</option>
						{/each}
					</select>
				</div>
				<div>
					<label for="incident-policy" class="label">Escalation policy</label>
					<select id="incident-policy" name="escalation_policy_id" class="select">
						<option value="">Project default</option>
						{#each data.policies as policy (policy.id)}
							<option value={policy.id}>{policy.name}{policy.is_default ? ' (default)' : ''}</option>
						{/each}
					</select>
				</div>
				<div class="sm:col-span-2">
					<button type="submit" class="btn btn-primary">Create incident</button>
				</div>
			</form>
		</details>
	{/if}

	<Card title="Incidents" padded={false}>
		{#snippet tabs()}
			<SegmentedTabs label="Incident status" items={tabItems} />
		{/snippet}
		{#snippet actions()}
			<!-- A plain GET form: the source is a query value like the tab. -->
			<form method="GET" class="flex items-center gap-2">
				<input type="hidden" name="tab" value={data.query.tab} />
				<label for="incident-source" class="sr-only">Source</label>
				<select id="incident-source" name="source" class="select py-1 text-xs">
					<option value="">All sources</option>
					{#each SOURCES as source (source)}
						<option value={source} selected={data.query.source === source}>{SOURCE_LABELS[source]}</option>
					{/each}
				</select>
				<button type="submit" class="btn btn-secondary btn-sm">Filter</button>
			</form>
		{/snippet}

		{#if data.incidents.length === 0}
			<EmptyState icon={data.query.tab === 'unresolved' ? 'heartbeat' : 'incident'} message={EMPTY[data.query.tab]} />
		{:else}
			<div class="overflow-x-auto">
				<table class="table">
					<thead>
						<tr>
							<th>Severity</th>
							<th>Incident</th>
							<th>Source</th>
							<th>Started</th>
							<th>Duration</th>
							<th>Acknowledged by</th>
							<th>Status</th>
						</tr>
					</thead>
					<tbody>
						{#each data.incidents as incident (incident.id)}
							{@const href = sourceLink(data.project.id, incident)}
							<tr class="align-top">
								<td><SeverityBadge name={incident.severity} rank={incident.severity_rank} /></td>
								<!-- Titles and causes carry text from the target (a name, an error
								     message) with no spaces to wrap at: break anywhere. -->
								<td class="min-w-64 space-y-1">
									<a href="/projects/{data.project.id}/incidents/{incident.id}" class="font-medium wrap-anywhere text-ink hover:text-primary">{incident.title}</a>
									{#if incident.cause}
										<p class="text-xs wrap-anywhere text-ink-2">{incident.cause}</p>
									{/if}
									{#if incident.details}
										<!-- Job output from a failing heartbeat: text only. -->
										<details>
											<summary class="cursor-pointer text-xs text-primary">Job output</summary>
											<pre class="mt-2 max-h-64 overflow-auto rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all whitespace-pre-wrap text-ink-2">{incident.details}</pre>
										</details>
									{/if}
								</td>
								<td class="wrap-anywhere">
									{#if href}
										<a {href} class="link">{sourceLabel(incident)}</a>
									{:else}
										{sourceLabel(incident)}
									{/if}
								</td>
								<td class="whitespace-nowrap">{formatUtc(incident.started_at)}</td>
								<td class="whitespace-nowrap">{duration(incident.started_at, incident.resolved_at, new Date(now))}</td>
								<td class="wrap-anywhere">{incident.acknowledged_by_email ?? '—'}</td>
								<td><span class="badge {STATUS_BADGE[incident.status].class}">{STATUS_BADGE[incident.status].label}</span></td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
		{#if data.nextCursor || data.query.cursor}
			<div class="flex flex-wrap gap-2 border-t border-border px-5 py-3 sm:px-6">
				{#if data.query.cursor}
					<a href={listHref(data.query.tab, data.query.source)} class="btn btn-secondary btn-sm">Newest</a>
				{/if}
				{#if data.nextCursor}
					<a href={listHref(data.query.tab, data.query.source, data.nextCursor)} class="btn btn-secondary btn-sm">Load more</a>
				{/if}
			</div>
		{/if}
	</Card>
</div>
