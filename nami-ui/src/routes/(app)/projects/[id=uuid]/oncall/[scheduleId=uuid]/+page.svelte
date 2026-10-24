<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import ScheduleForm from '#lib/components/ScheduleForm.svelte';
	import Card from '#lib/components/ui/Card.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import { formatInZone, shiftRows } from '#lib/incidents/oncall';
	import { WEEKDAYS } from '#lib/uptime/maintenance';
	import { formatUtc } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const schedule = $derived(data.schedule);
	const zone = $derived(schedule.timezone);
	const isAdmin = $derived(data.role === 'admin');
	const rows = $derived(shiftRows(data.shifts, zone));
	const rotation = $derived(
		schedule.rotation === 'custom' ? `Every ${schedule.rotation_hours} hours` : schedule.rotation === 'daily' ? 'Daily' : 'Weekly'
	);
	const days = (weekdays: number[]) => weekdays.map((d) => WEEKDAYS.find((w) => w.iso === d)?.label ?? d).join(', ');
	const NOT_MEMBER = 'not a project member: never paged';

	// Buttons stay disabled from a submit until its result is applied.
	let busy = $state(false);
	const submit: SubmitFunction = () => {
		busy = true;
		return async ({ update, result }) => {
			try {
				await update({ reset: result.type === 'success' });
			} finally {
				busy = false;
			}
		};
	};
	// Deletes ask with confirm() first, then send `confirm=yes`. Without
	// JavaScript the action asks on the page (the panels below).
	const asking =
		(question: (f: FormData) => string): SubmitFunction =>
		(submission) => {
			if (!confirm(question(submission.formData))) {
				submission.cancel();
				return;
			}
			submission.formData.set('confirm', 'yes');
			return submit(submission);
		};
	const askOverride = asking((f) => `Delete the override for ${f.get('email')}?`);
	const askSchedule = asking(() => `Delete schedule "${schedule.name}"?`);

	const pendingOverride = $derived(
		form?.action === 'deleteOverride' && 'confirmDeleteOverride' in form
			? data.overrides.find((o) => o.id === form.confirmDeleteOverride)
			: undefined
	);
	const message = (action: string) => (form?.action === action && 'message' in form ? form.message : null);
	const failed = (action: string) => form?.action === action && 'message' in form && !('saved' in form || 'added' in form || 'deleted' in form);
</script>

<svelte:head>
	<title>{schedule.name} · On-call · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<div>
		<a href="/projects/{data.project.id}/oncall" class="link text-sm">← On-call</a>
		<h2 class="mt-2 text-lg font-light wrap-anywhere text-ink">{schedule.name}</h2>
		<p class="mt-1 text-sm text-muted">
			{#if schedule.on_call_now}
				On call now: <span class="text-ink">{schedule.on_call_now.email}</span> until
				<span title={formatUtc(schedule.on_call_now.until)}>{formatInZone(schedule.on_call_now.until, zone)}</span> ({zone})
			{:else}
				Nobody is on call now.
			{/if}
		</p>
	</div>

	{#if isAdmin}
		<Card title="Schedule">
			{#if form?.action === 'save' && 'saved' in form}
				<div class="alert alert-success mb-4" role="status">{form.message}</div>
			{/if}
			<ScheduleForm {schedule} members={data.members} action="?/save" submitLabel="Save schedule" message={failed('save') ? message('save') : null} />
		</Card>
	{:else}
		<Card title="Schedule" subtitle="Only project admins can change on-call schedules.">
			<dl class="grid gap-3 text-sm sm:grid-cols-2">
				<div><dt class="text-muted">Rotation</dt><dd class="text-ink">{rotation}</dd></div>
				<div><dt class="text-muted">Handoff</dt><dd class="text-ink">{schedule.handoff_time} ({zone})</dd></div>
				<div><dt class="text-muted">Start date</dt><dd class="text-ink">{schedule.start_date}</dd></div>
				<div>
					<dt class="text-muted">Restrictions</dt>
					<dd class="text-ink">
						{#if schedule.restrictions.length === 0}
							None, all hours
						{:else}
							{#each schedule.restrictions as r, i (i)}
								<span class="block">{days(r.weekdays)} {r.from}–{r.to}</span>
							{/each}
						{/if}
					</dd>
				</div>
			</dl>
			<h3 class="mt-5 text-sm font-medium text-ink">Participants, in rotation order</h3>
			<ol class="mt-2 list-decimal space-y-1 pl-5 text-sm text-ink">
				{#each schedule.participants as participant (participant.user_id)}
					<li class="wrap-anywhere">
						{participant.email}
						{#if !participant.is_member}<span class="badge badge-warning ml-2">{NOT_MEMBER}</span>{/if}
					</li>
				{/each}
			</ol>
		</Card>
	{/if}

	<Card title="Next 14 days" subtitle="Times in {zone}; hover for UTC." padded={false}>
		{#if rows.length === 0}
			<EmptyState icon="calendar" message="Nobody is on call in the next 14 days." />
		{:else}
			<div class="overflow-x-auto">
				<table class="table table-compact">
					<thead>
						<tr><th>Who</th><th>From</th><th>Until</th><th></th></tr>
					</thead>
					<tbody>
						{#each rows as row, i (i)}
							<tr>
								<td class="wrap-anywhere">
									{row.who}
									{#if !data.shifts[i].is_member}<span class="badge badge-warning ml-2">{NOT_MEMBER}</span>{/if}
								</td>
								<td title={formatUtc(data.shifts[i].starts_at)}>{row.from}</td>
								<td title={formatUtc(data.shifts[i].ends_at)}>{row.to}</td>
								<td>{#if row.isOverride}<span class="badge badge-info">Override</span>{/if}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</Card>

	<Card title="Overrides" subtitle="An override puts someone on call regardless of the rotation; the newest wins on overlap.">
		{#if (form?.action === 'deleteOverride' || form?.action === 'addOverride') && message(form.action)}
			<div class="alert {failed(form.action) ? 'alert-danger' : 'alert-success'} mb-4" role={failed(form.action) ? 'alert' : 'status'}>
				{message(form.action)}
			</div>
		{/if}
		{#if isAdmin && pendingOverride}
			<form method="POST" action="?/deleteOverride" use:enhance={submit} class="alert alert-danger mb-4 flex flex-wrap items-center gap-3">
				<input type="hidden" name="override_id" value={pendingOverride.id} />
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">Delete the override for {pendingOverride.email}?</span>
				<button type="submit" disabled={busy} class="btn btn-danger-solid btn-sm">Yes, delete</button>
				<a href="?" class="text-ink-2 hover:text-ink">Cancel</a>
			</form>
		{/if}

		{#if data.overrides.length === 0}
			<p class="text-sm text-muted">No overrides.</p>
		{:else}
			<ul class="divide-y divide-border">
				{#each data.overrides as o (o.id)}
					<li class="flex flex-wrap items-center gap-3 py-3">
						<div class="min-w-0 flex-1 text-sm">
							<p class="wrap-anywhere text-ink">{o.email}</p>
							<p class="text-xs text-muted">
								<span title={formatUtc(o.starts_at)}>{formatInZone(o.starts_at, zone)}</span> →
								<span title={formatUtc(o.ends_at)}>{formatInZone(o.ends_at, zone)}</span> ({zone})
							</p>
						</div>
						{#if isAdmin}
							<form method="POST" action="?/deleteOverride" use:enhance={askOverride}>
								<input type="hidden" name="override_id" value={o.id} />
								<input type="hidden" name="email" value={o.email} />
								<button type="submit" disabled={busy} class="btn btn-danger btn-sm">Delete</button>
							</form>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}

		{#if isAdmin}
			<form method="POST" action="?/addOverride" use:enhance={submit} class="mt-5 grid gap-4 border-t border-border pt-5 sm:grid-cols-3">
				<input type="hidden" name="timezone" value={zone} />
				<div>
					<label for="override-user" class="label">Who takes over</label>
					<select id="override-user" name="user_id" required class="input">
						<option value="">Pick a member…</option>
						{#each data.members as member (member.user_id)}
							<option value={member.user_id}>{member.email}</option>
						{/each}
					</select>
				</div>
				<div>
					<label for="override-starts" class="label">Starts ({zone})</label>
					<input id="override-starts" name="starts_at" type="datetime-local" required class="input" />
				</div>
				<div>
					<label for="override-ends" class="label">Ends ({zone})</label>
					<input id="override-ends" name="ends_at" type="datetime-local" required class="input" />
				</div>
				<div class="sm:col-span-3">
					<button type="submit" disabled={busy} class="btn btn-primary btn-sm">Add override</button>
				</div>
			</form>
		{/if}
	</Card>

	{#if isAdmin}
		<Card title="Delete schedule">
			{#if failed('delete')}
				<div class="alert alert-danger mb-4" role="alert">{message('delete')}</div>
			{/if}
			{#if form?.action === 'delete' && 'confirmDelete' in form}
				<form method="POST" action="?/delete" use:enhance={submit} class="alert alert-danger flex flex-wrap items-center gap-3">
					<input type="hidden" name="confirm" value="yes" />
					<span class="wrap-anywhere">Delete schedule "{schedule.name}"?</span>
					<button type="submit" disabled={busy} class="btn btn-danger-solid btn-sm">Yes, delete</button>
					<a href="?" class="text-ink-2 hover:text-ink">Cancel</a>
				</form>
			{:else}
				<form method="POST" action="?/delete" use:enhance={askSchedule} class="flex flex-wrap items-center gap-3">
					<p class="flex-1 text-sm text-muted">A schedule that an escalation policy targets cannot be deleted.</p>
					<button type="submit" disabled={busy} class="btn btn-danger btn-sm">Delete schedule</button>
				</form>
			{/if}
		</Card>
	{/if}
</div>
