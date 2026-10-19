<script lang="ts">
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import { enhance, type SubmitFunction } from '$app/forms';
	import { refreshAll } from '$app/navigation';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import Icon from '#lib/components/ui/Icon.svelte';
	import { formatSeconds, groupMonitors, pingUrl, timeAgo } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	// Re-run the load every 30 s, and tick a clock so "last ping" stays current
	// in between (same as the monitor list).
	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});

	// The Create button stays disabled from the submit until its result is
	// applied, so a double click cannot create two heartbeats.
	let creating = $state(false);
	const create: SubmitFunction = () => {
		creating = true;
		return async ({ update }) => {
			try {
				await update();
			} finally {
				creating = false;
			}
		};
	};

	const sections = $derived(groupMonitors(data.heartbeats, data.groups));
	const grouped = $derived(data.groups.length > 0);

	const input = 'input';
	const label = 'label';
</script>

{#snippet rows(heartbeats: typeof data.heartbeats)}
			<ul>
				{#each heartbeats as heartbeat (heartbeat.id)}
					<!-- The engine sends no token to a viewer: no ping URL then. -->
					{@const url = heartbeat.token ? pingUrl(data.engineUrl, heartbeat.token) : null}
					<li class="space-y-2 border-b border-border px-6 py-4 last:border-0">
						<div class="flex flex-wrap items-center justify-between gap-2">
							<div class="flex items-center gap-4">
								<StatusBadge status={heartbeat.current_status} paused={heartbeat.paused} />
								{#if heartbeat.in_maintenance}<MaintenanceBadge />{/if}
								<a
									href="/projects/{data.project.id}/heartbeats/{heartbeat.id}"
									class="font-medium wrap-anywhere text-ink hover:text-primary">{heartbeat.name}</a
								>
							</div>
							<p class="text-xs text-muted">
								every {formatSeconds(heartbeat.period_seconds)} · grace {formatSeconds(heartbeat.grace_seconds)} ·
								last ping {timeAgo(heartbeat.last_ping_at, now)}
							</p>
						</div>
						{#if url}
							<div role="group" aria-label="Ping URL of {heartbeat.name}" class="flex items-center gap-2">
								<code
									class="block flex-1 truncate rounded-control border border-border bg-surface-2 px-3 py-1.5 font-mono text-xs text-ink-2 select-all"
									>{url}</code
								>
								<CopyButton value={url} />
							</div>
						{:else}
							<p class="text-xs text-muted">Ping URL is visible to project members.</p>
						{/if}
					</li>
				{/each}
			</ul>
{/snippet}

<svelte:head>
	<title>Heartbeats · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<div class="card">
		<div class="border-b border-border px-5 py-4 sm:px-6">
			<h2 class="text-lg font-light text-ink">Heartbeats</h2>
			<p class="text-xs text-muted">
				Cron jobs ping their URL after each run; a heartbeat goes down when no ping arrives within its period plus
				grace. Refreshes every 30 seconds.
			</p>
		</div>
		{#if data.heartbeats.length === 0}
			<EmptyState icon="heartbeat" message="No heartbeats yet — create one below." />
		{:else}
			{#if !grouped}
				{@render rows(data.heartbeats)}
			{:else}
				{#each sections as section (section.group?.id ?? 'ungrouped')}
					{@const down = section.monitors.filter((h) => h.current_status === 'down' && !h.paused).length}
					<details open class="group/section border-b border-border last:border-0">
						<summary
							class="flex cursor-pointer list-none items-center gap-3 bg-surface-2 px-5 py-3 text-sm hover:bg-primary-soft sm:px-6 [&::-webkit-details-marker]:hidden"
						>
							<Icon name="chevronDown" size={16} class="-rotate-90 text-muted transition-transform group-open/section:rotate-0" />
							<span class="font-medium wrap-anywhere text-ink">{section.group?.name ?? 'Ungrouped'}</span>
							<span class="text-xs text-muted">
								{section.monitors.length} heartbeat{section.monitors.length === 1 ? '' : 's'}
							</span>
							{#if down > 0}<span class="badge badge-danger">{down} down</span>{/if}
						</summary>
						{@render rows(section.monitors)}
					</details>
				{/each}
			{/if}
		{/if}
	</div>

	<div class="card p-5 sm:p-6">
		<h2 class="text-lg font-light text-ink">New heartbeat</h2>
		{#if form?.message}
			<div role="alert" class="alert alert-danger mt-4">
				{form.message}
			</div>
		{/if}
		<form method="POST" use:enhance={create} class="mt-4 grid gap-4 sm:grid-cols-[1fr_10rem_10rem_auto] sm:items-end">
			<div>
				<label for="hb-name" class={label}>Name</label>
				<input id="hb-name" name="name" type="text" required maxlength="200" placeholder="Nightly backup" class={input} />
			</div>
			<div>
				<label for="hb-period" class={label}>Period (seconds)</label>
				<input
					id="hb-period"
					name="period_seconds"
					type="number"
					required
					min="30"
					max="2678400"
					aria-describedby="hb-hint"
					value="86400"
					class={input}
				/>
			</div>
			<div>
				<label for="hb-grace" class={label}>Grace (seconds)</label>
				<input
					id="hb-grace"
					name="grace_seconds"
					type="number"
					min="0"
					max="2678400"
					aria-describedby="hb-hint"
					value="60"
					class={input}
				/>
			</div>
			{#if data.groups.length > 0}
				<div class="sm:col-span-3 sm:max-w-sm">
					<label for="hb-group" class={label}>Group</label>
					<select id="hb-group" name="group_id" class="select">
						<option value="">No group</option>
						{#each data.groups as group (group.id)}
							<option value={group.id}>{group.name}</option>
						{/each}
					</select>
				</div>
			{/if}
			<button
				type="submit"
				disabled={creating}
				class="btn btn-primary">
				Create
			</button>
			<p id="hb-hint" class="text-xs text-muted sm:col-span-4">
				Period: at least 30, grace: 0 or more, both at most 2678400 (31 days).
			</p>
		</form>
	</div>
</div>
