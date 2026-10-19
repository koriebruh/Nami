<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import { onMount } from 'svelte';
	import { WEEKDAYS, shortTime, toUtcInputValue, type MaintenanceWindow, type WindowKind } from '#lib/maintenance';

	let {
		maintenanceWindow = null,
		monitors,
		submitLabel,
		message = null,
		action = undefined,
		cancelHref = null
	}: {
		maintenanceWindow?: MaintenanceWindow | null;
		monitors: { id: string; name: string }[];
		submitLabel: string;
		message?: string | null;
		action?: string;
		cancelHref?: string | null;
	} = $props();

	// Read once, in functions, on purpose: after mount the inputs own them.
	function initialKind(): WindowKind {
		return maintenanceWindow?.kind ?? 'once';
	}
	function initialTimezone(): string {
		return maintenanceWindow?.timezone ?? 'UTC';
	}
	function isNew(): boolean {
		return maintenanceWindow === null;
	}
	function initialScope(): string {
		return maintenanceWindow?.monitor_id ?? '';
	}
	function browserZone(): string {
		return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
	}
	let kind = $state<WindowKind>(initialKind());
	let scope = $state(initialScope());
	let timezone = $state(initialTimezone());
	// Suggestions only: the engine is the authority on what a zone is.
	let zones = $state<string[]>(['UTC']);

	onMount(() => {
		try {
			zones = ['UTC', ...Intl.supportedValuesOf('timeZone').filter((zone) => zone !== 'UTC')];
		} catch {
			// An older browser: the field still takes any IANA name typed in.
		}
		// A new weekly window starts out in the browser's time zone.
		if (isNew()) timezone = browserZone();
	});

	// The submit button stays disabled from the submit until the result is
	// applied, so a double click cannot create two windows. It is in the
	// `actions` snippet on purpose (see MonitorForm). The inputs are never
	// reset (a reset would untick the bound radios): a refusal keeps what was
	// typed, and a saved window remounts the form, empty, via `generation`.
	let submitting = $state(false);
	let generation = $state(0);
	const submit: SubmitFunction = () => {
		submitting = true;
		return async ({ update, result }) => {
			try {
				await update({ reset: false });
				if (result.type === 'success') {
					kind = initialKind();
					scope = initialScope();
					timezone = isNew() ? browserZone() : initialTimezone();
					generation += 1;
				}
			} finally {
				submitting = false;
			}
		};
	};

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
	const choice = 'flex items-center gap-2 text-sm text-neutral-300';
</script>

<!-- Initial values go through defaultValue/defaultChecked (or a binding),
     never value={...}: see MonitorForm. -->
{#key generation}
<form method="POST" {action} use:enhance={submit} class="space-y-4">
	{#if message}
		<div role="alert" class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">{message}</div>
	{/if}

	<div class="grid gap-4 sm:grid-cols-2">
		<div>
			<label for="mw-title" class={label}>Title</label>
			<input id="mw-title" name="title" type="text" required maxlength="200" placeholder="Database upgrade" defaultValue={maintenanceWindow?.title ?? ''} class={input} />
		</div>
		<div>
			<label for="mw-scope" class={label}>Covers</label>
			<select id="mw-scope" name="monitor_id" bind:value={scope} class={input}>
				<option value="">All monitors and heartbeats</option>
				{#each monitors as monitor (monitor.id)}
					<option value={monitor.id}>{monitor.name}</option>
				{/each}
			</select>
		</div>
	</div>

	<fieldset class="flex flex-wrap gap-6">
		<legend class="sr-only">Repeats</legend>
		<label class={choice}><input type="radio" name="kind" value="once" bind:group={kind} /> One time</label>
		<label class={choice}><input type="radio" name="kind" value="weekly" bind:group={kind} /> Every week</label>
	</fieldset>

	{#if kind === 'once'}
		<div class="grid gap-4 sm:grid-cols-2">
			<div>
				<label for="mw-starts" class={label}>Starts (UTC)</label>
				<input id="mw-starts" name="starts_at" type="datetime-local" required defaultValue={toUtcInputValue(maintenanceWindow?.starts_at ?? null)} class={input} />
			</div>
			<div>
				<label for="mw-ends" class={label}>Ends (UTC)</label>
				<input id="mw-ends" name="ends_at" type="datetime-local" required defaultValue={toUtcInputValue(maintenanceWindow?.ends_at ?? null)} class={input} />
			</div>
		</div>
	{:else}
		<fieldset>
			<legend class={label}>Days</legend>
			<div class="flex flex-wrap gap-4">
				{#each WEEKDAYS as day (day.iso)}
					<label class={choice}>
						<input type="checkbox" name="days_of_week" defaultValue={day.iso} defaultChecked={maintenanceWindow?.days_of_week?.includes(day.iso) ?? false} />
						{day.label}
					</label>
				{/each}
			</div>
		</fieldset>
		<div class="grid gap-4 sm:grid-cols-3">
			<div>
				<label for="mw-start-time" class={label}>From</label>
				<input id="mw-start-time" name="start_time" type="time" required defaultValue={shortTime(maintenanceWindow?.start_time ?? null)} class={input} />
			</div>
			<div>
				<label for="mw-end-time" class={label}>Until</label>
				<input id="mw-end-time" name="end_time" type="time" required defaultValue={shortTime(maintenanceWindow?.end_time ?? null)} class={input} />
			</div>
			<div>
				<label for="mw-timezone" class={label}>Time zone</label>
				<input id="mw-timezone" name="timezone" type="text" required list="mw-timezones" bind:value={timezone} class={input} />
				<datalist id="mw-timezones">
					{#each zones as zone (zone)}
						<option value={zone}></option>
					{/each}
				</datalist>
			</div>
		</div>
		<p class="text-xs text-neutral-500">An end at or before the start runs past midnight into the next day.</p>
	{/if}

	{@render actions()}
</form>
{/key}

{#snippet actions()}
	<div class="flex items-center gap-3">
		<button
			type="submit"
			disabled={submitting}
			class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
		>
			{submitLabel}
		</button>
		{#if cancelHref}<a href={cancelHref} class="text-sm text-neutral-500 hover:text-neutral-300">Cancel</a>{/if}
	</div>
{/snippet}
