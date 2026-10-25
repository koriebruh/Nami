<script lang="ts">
	import ScheduleForm from '#lib/components/ScheduleForm.svelte';
	import Card from '#lib/components/ui/Card.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import { formatInZone } from '#lib/incidents/oncall';
	import { formatUtc } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const ROTATIONS = { daily: 'Daily', weekly: 'Weekly', custom: 'Custom' } as const;
	const rotationLabel = (s: (typeof data.schedules)[number]) =>
		s.rotation === 'custom' ? `Every ${s.rotation_hours} h` : (ROTATIONS[s.rotation] ?? s.rotation);
</script>

<svelte:head>
	<title>On-call · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<Card title="On-call schedules" subtitle="Times in each schedule's own time zone; hover for UTC." padded={false}>
		{#if data.schedules.length === 0}
			<EmptyState icon="calendar" message="No on-call schedules yet." />
		{:else}
			<div class="overflow-x-auto">
				<table class="table table-hover">
					<thead>
						<tr>
							<th>Name</th>
							<th>On call now</th>
							<th>Until</th>
							<th>Rotation</th>
							<th>Time zone</th>
						</tr>
					</thead>
					<tbody>
						{#each data.schedules as schedule (schedule.id)}
							<tr>
								<td><a href="/projects/{data.project.id}/oncall/{schedule.id}" class="link wrap-anywhere">{schedule.name}</a></td>
								<td class="wrap-anywhere">{schedule.on_call_now?.email ?? 'Nobody'}</td>
								<td>
									{#if schedule.on_call_now}
										<span title={formatUtc(schedule.on_call_now.until)}>{formatInZone(schedule.on_call_now.until, schedule.timezone)}</span>
									{:else}
										<span class="text-muted">–</span>
									{/if}
								</td>
								<td>{rotationLabel(schedule)}</td>
								<td class="text-muted">{schedule.timezone}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</Card>

	{#if data.role === 'admin'}
		<Card title="New schedule">
			<ScheduleForm
				members={data.members}
				action="?/create"
				submitLabel="Create schedule"
				message={form?.action === 'create' ? form.message : null}
			/>
		</Card>
	{:else}
		<p class="text-sm text-muted">Only project admins can change on-call schedules.</p>
	{/if}
</div>
