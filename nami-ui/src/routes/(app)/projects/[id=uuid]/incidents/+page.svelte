<script lang="ts">
	import { refreshAll } from '$app/navigation';
	import { formatDuration, formatUtc } from '#lib/uptime';
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

	// An incident belongs to a monitor or to a heartbeat (one of the two ids is null).
	function sourceHref(incident: (typeof data.incidents)[number]): string | null {
		if (incident.monitor_id) return `/projects/${data.project.id}/monitors/${incident.monitor_id}`;
		if (incident.heartbeat_id) return `/projects/${data.project.id}/heartbeats/${incident.heartbeat_id}`;
		return null;
	}
</script>

<svelte:head>
	<title>Incidents · {data.project.name}</title>
</svelte:head>

<div class="rounded-xl border border-[#262626] bg-[#141414]">
	<div class="flex items-center justify-between border-b border-[#262626] px-6 py-4">
		<h2 class="text-base font-semibold text-neutral-100">Incidents</h2>
		<div class="flex gap-1 rounded-md border border-[#262626] bg-[#0a0a0a] p-0.5">
			{#each tabs as tab (tab.status)}
				<a
					href="?status={tab.status}"
					aria-current={data.status === tab.status ? 'page' : undefined}
					class="rounded px-3 py-1 text-sm transition-colors {data.status === tab.status
						? 'bg-[#262626] text-neutral-100'
						: 'text-neutral-500 hover:text-neutral-300'}">{tab.label}</a
				>
			{/each}
		</div>
	</div>

	{#if data.incidents.length === 0}
		<p class="px-6 py-10 text-center text-sm text-neutral-500">
			{data.status === 'open' ? 'No open incidents — everything is up.' : 'No resolved incidents yet.'}
		</p>
	{:else}
		<ul>
			{#each data.incidents as incident (incident.id)}
				{@const href = sourceHref(incident)}
				<li class="flex flex-wrap items-start justify-between gap-3 border-b border-[#262626] px-6 py-4 last:border-0">
					<div class="space-y-1">
						<!-- Titles and causes carry text from the target (a name, an error
						     message) with no spaces to wrap at: break anywhere. -->
						<p class="text-sm font-medium wrap-anywhere text-neutral-100">
							{#if href}
								<a {href} class="hover:text-teal-300">{incident.title}</a>
							{:else}
								{incident.title}
							{/if}
						</p>
						{#if incident.cause}
							<p class="text-sm wrap-anywhere text-neutral-400">{incident.cause}</p>
						{/if}
						<p class="text-xs text-neutral-500">
							Started {formatUtc(incident.started_at)}
							{#if incident.resolved_at}· resolved {formatUtc(incident.resolved_at)}{/if}
						</p>
					</div>
					<span
						class="rounded-full border px-2 py-0.5 text-xs {incident.status === 'open'
							? 'border-red-500/30 bg-red-500/10 text-red-400'
							: 'border-[#262626] text-neutral-400'}"
					>
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
</div>
