<script lang="ts">
	import { refreshAll } from '$app/navigation';
	import Card from '#lib/components/ui/Card.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import Icon from '#lib/components/ui/Icon.svelte';
	import SegmentedTabs from '#lib/components/ui/SegmentedTabs.svelte';
	import { isUuid } from '#lib/core/uuid';
	import { formatDuration, formatUtc } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});

	const tabs = [
		{ status: 'open', label: 'Open' },
		{ status: 'resolved', label: 'Resolved' }
	] as const;
	const tabItems = $derived(
		tabs.map((tab) => ({ href: `?status=${tab.status}`, label: tab.label, active: data.status === tab.status }))
	);

	// An incident belongs to a monitor, a heartbeat or a log alert (the other ids are null).
	function sourceHref(incident: (typeof data.incidents)[number]): string | null {
		if (incident.monitor_id) return `/projects/${data.project.id}/monitors/${incident.monitor_id}`;
		if (incident.heartbeat_id) return `/projects/${data.project.id}/heartbeats/${incident.heartbeat_id}`;
		if (isUuid(incident.log_alert_id)) return `/projects/${data.project.id}/logs/alerts/${incident.log_alert_id}`;
		return null;
	}
</script>

<svelte:head>
	<title>Incidents · {data.project.name}</title>
</svelte:head>

<Card title="Incidents" padded={false}>
	{#snippet tabs()}
		<SegmentedTabs label="Incident status" items={tabItems} />
	{/snippet}

	{#if data.incidents.length === 0}
		<EmptyState
			icon={data.status === 'open' ? 'heartbeat' : 'incident'}
			message={data.status === 'open' ? 'No open incidents — everything is up.' : 'No resolved incidents yet.'}
		/>
	{:else}
		<ul class="divide-y divide-border">
			{#each data.incidents as incident (incident.id)}
				{@const href = sourceHref(incident)}
				<li class="flex flex-wrap items-start justify-between gap-3 px-5 py-4 sm:px-6">
					<div class="flex min-w-0 gap-3">
						<span
							class="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-full {incident.status === 'open'
								? 'bg-danger-soft text-danger-ink'
								: 'bg-success-soft text-success-ink'}"
							aria-hidden="true"
						>
							<Icon name={incident.status === 'open' ? 'incident' : 'heartbeat'} size={16} />
						</span>
						<div class="min-w-0 space-y-1">
							<!-- Titles and causes carry text from the target (a name, an error
							     message) with no spaces to wrap at: break anywhere. -->
							<p class="text-sm font-medium wrap-anywhere text-ink">
								{#if href}
									<a {href} class="hover:text-primary">{incident.title}</a>
								{:else}
									{incident.title}
								{/if}
							</p>
							{#if incident.cause}
								<p class="text-sm wrap-anywhere text-ink-2">{incident.cause}</p>
							{/if}
							{#if incident.details}
								<!-- Job output from a failing heartbeat: text only. -->
								<details>
									<summary class="cursor-pointer text-xs text-primary">Job output</summary>
									<pre class="mt-2 max-h-64 overflow-auto rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all whitespace-pre-wrap text-ink-2">{incident.details}</pre>
								</details>
							{/if}
							<p class="text-xs text-muted">
								Started {formatUtc(incident.started_at)}
								{#if incident.resolved_at}· resolved {formatUtc(incident.resolved_at)}{/if}
							</p>
						</div>
					</div>
					<span class="badge {incident.status === 'open' ? 'badge-danger' : 'badge-neutral'}">
						{incident.status === 'open' ? 'Ongoing' : 'Resolved'} · {formatDuration(
							incident.started_at,
							incident.resolved_at,
							now
						)}
					</span>
				</li>
			{/each}
		</ul>
	{/if}
</Card>
