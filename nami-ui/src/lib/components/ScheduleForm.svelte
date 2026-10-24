<!-- On-call schedule editor, for a new schedule (list page) and an existing
     one (detail page). Participants are sent in their listed order as
     repeated `participant` fields; restrictions as `restriction_<i>_*`. -->
<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import { onMount } from 'svelte';
	import { WEEKDAYS } from '#lib/uptime/maintenance';
	import type { Member, Participant, Restriction, Schedule } from '#lib/incidents/oncall';

	let {
		schedule = null,
		members,
		action,
		submitLabel,
		message = null
	}: {
		schedule?: Schedule | null;
		members: Member[];
		action: string;
		submitLabel: string;
		message?: string | null;
	} = $props();

	// Read once, in functions, on purpose: after mount the inputs own them. A
	// saved form remounts (`generation`) and reads them again.
	function initialRotation() {
		return schedule?.rotation ?? 'weekly';
	}
	function initialParticipants(): Participant[] {
		return (schedule?.participants ?? []).map((p) => ({ ...p }));
	}
	function initialRestrictions(): Restriction[] {
		return (schedule?.restrictions ?? []).map((r) => ({ ...r, weekdays: [...r.weekdays] }));
	}
	function initialTimezone() {
		return schedule?.timezone ?? 'UTC';
	}

	let rotation = $state(initialRotation());
	let participants = $state(initialParticipants());
	let restrictions = $state(initialRestrictions());
	let timezone = $state(initialTimezone());
	let pick = $state('');
	let zones = $state<string[]>(['UTC']);

	const addable = $derived(members.filter((m) => !participants.some((p) => p.user_id === m.user_id)));

	onMount(() => {
		try {
			zones = ['UTC', ...Intl.supportedValuesOf('timeZone').filter((zone) => zone !== 'UTC')];
		} catch {
			// An older browser: the field still takes any IANA name typed in.
		}
	});

	function add() {
		const member = members.find((m) => m.user_id === pick);
		if (!member) return;
		participants.push({ user_id: member.user_id, email: member.email, is_member: true });
		pick = '';
	}
	function move(i: number, by: -1 | 1) {
		const j = i + by;
		if (j < 0 || j >= participants.length) return;
		[participants[i], participants[j]] = [participants[j], participants[i]];
	}

	let submitting = $state(false);
	let generation = $state(0);
	const submit: SubmitFunction = () => {
		submitting = true;
		return async ({ update, result }) => {
			try {
				await update({ reset: false });
				if (result.type === 'success') {
					rotation = initialRotation();
					participants = initialParticipants();
					restrictions = initialRestrictions();
					timezone = initialTimezone();
					generation += 1;
				}
			} finally {
				submitting = false;
			}
		};
	};

	const id = (name: string) => `${schedule?.id ?? 'new'}-${name}`;
	const choice = 'flex items-center gap-2 text-sm text-ink-2';
</script>

{#key generation}
<form method="POST" {action} use:enhance={submit} class="space-y-5">
	{#if message}
		<div role="alert" class="alert alert-danger">{message}</div>
	{/if}

	<div class="grid gap-4 sm:grid-cols-2">
		<div>
			<label for={id('name')} class="label">Name</label>
			<input id={id('name')} name="name" type="text" required maxlength="100" placeholder="Primary" defaultValue={schedule?.name ?? ''} class="input" />
		</div>
		<div>
			<label for={id('timezone')} class="label">Time zone</label>
			<input id={id('timezone')} name="timezone" type="text" required list={id('zones')} bind:value={timezone} class="input" />
			<datalist id={id('zones')}>
				{#each zones as zone (zone)}
					<option value={zone}></option>
				{/each}
			</datalist>
		</div>
		<div>
			<label for={id('rotation')} class="label">Rotation</label>
			<select id={id('rotation')} name="rotation" bind:value={rotation} class="input">
				<option value="daily">Daily</option>
				<option value="weekly">Weekly</option>
				<option value="custom">Custom length</option>
			</select>
		</div>
		{#if rotation === 'custom'}
			<div>
				<label for={id('hours')} class="label">Rotation length (hours)</label>
				<input id={id('hours')} name="rotation_hours" type="number" required min="1" max="8760" step="1" defaultValue={schedule?.rotation_hours ?? 12} class="input" />
			</div>
		{/if}
		<div>
			<label for={id('handoff')} class="label">Handoff time</label>
			<input id={id('handoff')} name="handoff_time" type="time" required defaultValue={schedule?.handoff_time ?? '09:00'} class="input" />
		</div>
		<div>
			<label for={id('start')} class="label">Start date</label>
			<input id={id('start')} name="start_date" type="date" required defaultValue={schedule?.start_date ?? ''} class="input" />
		</div>
	</div>
	<p class="text-xs text-muted">The handoff time and the restrictions are wall-clock times in the schedule's time zone.</p>

	<fieldset class="space-y-2">
		<legend class="label">Participants, in rotation order</legend>
		{#if participants.length === 0}
			<p class="text-sm text-muted">Add at least one participant.</p>
		{/if}
		<ol class="space-y-2">
			{#each participants as participant, i (participant.user_id)}
				<li class="flex flex-wrap items-center gap-2 rounded-control border border-border px-3 py-2">
					<input type="hidden" name="participant" value={participant.user_id} />
					<span class="text-sm text-muted">{i + 1}.</span>
					<span class="min-w-0 flex-1 text-sm wrap-anywhere text-ink">
						{participant.email}
						{#if !participant.is_member}
							<span class="badge badge-warning ml-2">not a project member: never paged</span>
						{/if}
					</span>
					<button type="button" class="btn btn-ghost btn-sm" disabled={i === 0} onclick={() => move(i, -1)} aria-label="Move {participant.email} up">↑</button>
					<button type="button" class="btn btn-ghost btn-sm" disabled={i === participants.length - 1} onclick={() => move(i, 1)} aria-label="Move {participant.email} down">↓</button>
					<button type="button" class="btn btn-ghost btn-sm" onclick={() => participants.splice(i, 1)} aria-label="Remove {participant.email}">Remove</button>
				</li>
			{/each}
		</ol>
		<div class="flex flex-wrap items-end gap-2">
			<div class="min-w-[14rem] flex-1">
				<label for={id('pick')} class="sr-only">Member to add</label>
				<select id={id('pick')} bind:value={pick} class="input">
					<option value="">Add a member…</option>
					{#each addable as member (member.user_id)}
						<option value={member.user_id}>{member.email}</option>
					{/each}
				</select>
			</div>
			<button type="button" class="btn btn-secondary btn-sm" disabled={pick === ''} onclick={add}>Add</button>
		</div>
	</fieldset>

	<fieldset class="space-y-2">
		<legend class="label">Working-hours restrictions</legend>
		<p class="text-xs text-muted">None: the rotation covers all hours. Otherwise nobody is on call by rotation outside these windows (overrides still apply). An end at or before the start runs past midnight.</p>
		<input type="hidden" name="restriction_count" value={restrictions.length} />
		{#each restrictions as restriction, i (restriction)}
			<div class="space-y-2 rounded-control border border-border px-3 py-3">
				<div class="flex flex-wrap gap-4">
					{#each WEEKDAYS as day (day.iso)}
						<label class={choice}>
							<input type="checkbox" name="restriction_{i}_weekday" value={day.iso} bind:group={restriction.weekdays} />
							{day.label}
						</label>
					{/each}
				</div>
				<div class="flex flex-wrap items-end gap-3">
					<div>
						<label for={id(`r${i}-from`)} class="label">From</label>
						<input id={id(`r${i}-from`)} name="restriction_{i}_from" type="time" required bind:value={restriction.from} class="input" />
					</div>
					<div>
						<label for={id(`r${i}-to`)} class="label">Until</label>
						<input id={id(`r${i}-to`)} name="restriction_{i}_to" type="time" required bind:value={restriction.to} class="input" />
					</div>
					<button type="button" class="btn btn-ghost btn-sm" onclick={() => restrictions.splice(i, 1)}>Remove</button>
				</div>
			</div>
		{/each}
		{#if restrictions.length < 14}
			<button type="button" class="btn btn-secondary btn-sm" onclick={() => restrictions.push({ weekdays: [1, 2, 3, 4, 5], from: '09:00', to: '17:00' })}>
				Add restriction
			</button>
		{/if}
	</fieldset>

	<button type="submit" disabled={submitting} class="btn btn-primary">{submitLabel}</button>
</form>
{/key}
