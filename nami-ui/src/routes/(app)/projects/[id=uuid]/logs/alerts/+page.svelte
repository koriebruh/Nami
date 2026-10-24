<script lang="ts">
	import Badge from '#lib/components/ui/Badge.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import { describeCondition } from '#lib/logs/log-alerts';
	import { formatUtc } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const base = $derived(`/projects/${data.project.id}/logs/alerts`);
</script>

<svelte:head>
	<title>Alerts · Logs · {data.project.name}</title>
</svelte:head>

<div class="card">
	<div class="flex flex-wrap items-start justify-between gap-3 border-b border-border px-5 py-4 sm:px-6">
		<div>
			<h2 class="text-lg font-light text-ink">Log alerts</h2>
			<p class="text-xs text-muted">
				Counts matching events over a window and opens an incident when the condition holds. Times in UTC.
			</p>
		</div>
		<a href="{base}/new" class="btn btn-primary btn-sm">New alert</a>
	</div>
	{#if data.alerts.length === 0}
		<EmptyState icon="incident" message="No log alerts yet." />
	{:else}
		<div class="overflow-x-auto">
			<table class="table table-hover">
				<thead>
					<tr>
						<th>Name</th>
						<th>State</th>
						<th>Condition</th>
						<th>Last count</th>
						<th>Last evaluated</th>
					</tr>
				</thead>
				<tbody>
					{#each data.alerts as alert (alert.id)}
						<tr>
							<td>
								<a href="{base}/{alert.id}" class="font-medium wrap-anywhere text-ink hover:text-primary">{alert.name}</a>
								{#if alert.last_error}
									<p class="mt-1 text-xs wrap-anywhere text-danger-ink">
										Could not evaluate: {alert.last_error}{#if alert.last_error_at}
											({formatUtc(alert.last_error_at)}){/if}
									</p>
								{/if}
							</td>
							<td>
								<span class="flex flex-wrap gap-1">
									<Badge tone={alert.state === 'alerting' ? 'danger' : 'neutral'} dot>
										{alert.state === 'alerting' ? 'Alerting' : 'OK'}
									</Badge>
									{#if !alert.enabled}<Badge tone="neutral">Disabled</Badge>{/if}
								</span>
							</td>
							<td class="wrap-anywhere">{describeCondition(alert)}</td>
							<td class="tabular-nums">{alert.last_count === null ? '—' : alert.last_count.toLocaleString('en-US')}</td>
							<td>{alert.last_evaluated_at ? formatUtc(alert.last_evaluated_at) : 'Not yet'}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</div>
