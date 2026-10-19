<script lang="ts">
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import { enhance, type SubmitFunction } from '$app/forms';
	import { refreshAll } from '$app/navigation';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import { formatSeconds, pingUrl, timeAgo } from '#lib/uptime';
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

	const input = 'input';
	const label = 'label';
</script>

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
			<ul>
				{#each data.heartbeats as heartbeat (heartbeat.id)}
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
