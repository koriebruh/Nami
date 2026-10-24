<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import { onMount, tick } from 'svelte';
	import Card from '#lib/components/ui/Card.svelte';
	import { MAX_DELAY_MINUTES, MAX_REPEAT_COUNT, MAX_STEPS, MAX_TARGETS_PER_STEP, newStep, type EditorStep } from '#lib/incidents/escalation';
	import { WEEKDAYS } from '#lib/uptime/maintenance';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const policy = $derived(data.policy);
	const isAdmin = $derived(data.role === 'admin');
	const fallbackName = $derived(data.fallbacks.find((p) => p.id === policy.fallback_policy_id)?.name ?? null);
	const days = (weekdays: number[]) => weekdays.map((d) => WEEKDAYS.find((w) => w.iso === d)?.label ?? d).join(', ');
	const after = (minutes: number, i: number) =>
		minutes === 0 ? (i === 0 ? 'At once' : 'Right after the previous step') : `${minutes} min after ${i === 0 ? 'the incident opens' : 'the previous step'}`;

	// The editor owns its steps from here on (add, remove, move). Read in a
	// function on purpose: a successful save reads the saved policy again and
	// remounts the form (`generation`).
	type Row = EditorStep & { key: number; pick: string };
	let nextKey = 0;
	const row = (step: EditorStep): Row => ({
		...step,
		targets: step.targets.map((t) => ({ ...t })),
		weekdays: [...step.weekdays],
		key: nextKey++,
		pick: ''
	});
	function initialSteps(): Row[] {
		return data.editor.steps.map(row);
	}
	let steps = $state(initialSteps());
	let generation = $state(0);
	let zones = $state<string[]>(['UTC']);

	onMount(() => {
		try {
			zones = ['UTC', ...Intl.supportedValuesOf('timeZone').filter((zone) => zone !== 'UTC')];
		} catch {
			// An older browser: the field still takes any IANA name typed in.
		}
	});

	// Moving, adding and removing re-render the list: focus goes back where a
	// keyboard user expects it, and the change is announced (as ScheduleForm).
	let announcement = $state('');
	async function focusLater(elementId: string) {
		await tick();
		document.getElementById(elementId)?.focus();
	}
	function move(i: number, by: -1 | 1) {
		const j = i + by;
		if (j < 0 || j >= steps.length) return;
		[steps[i], steps[j]] = [steps[j], steps[i]];
		const moved = steps[j];
		announcement = `Step moved to position ${j + 1} of ${steps.length}.`;
		// At the top or bottom the pressed button is disabled; use the other one.
		const atEdge = by < 0 ? j === 0 : j === steps.length - 1;
		focusLater(id(`s${moved.key}-${by < 0 !== atEdge ? 'up' : 'down'}`));
	}
	function addStep() {
		const step = row(newStep());
		steps.push(step);
		announcement = `Step ${steps.length} added.`;
		focusLater(id(`s${step.key}-delay`));
	}
	function removeStep(i: number) {
		steps.splice(i, 1);
		announcement = `Step ${i + 1} removed.`;
		const next = steps[i] ?? steps[i - 1];
		focusLater(id(`s${next.key}-remove`));
	}
	function addTarget(step: Row, i: number) {
		const choice = data.choices.find((c) => c.value === step.pick);
		if (choice && !step.targets.some((t) => t.value === choice.value)) {
			step.targets.push({ ...choice });
			announcement = `${choice.label} added to step ${i + 1}.`;
		}
		step.pick = '';
		// The Add button is disabled now; the picker (or, at the target limit,
		// the last target) takes the focus.
		focusLater(step.targets.length < MAX_TARGETS_PER_STEP ? id(`s${step.key}-pick`) : id(`s${step.key}-t${step.targets.length - 1}`));
	}
	function removeTarget(step: Row, i: number, t: number) {
		const [removed] = step.targets.splice(t, 1);
		announcement = `${removed.label} removed from step ${i + 1}.`;
		const next = Math.min(t, step.targets.length - 1);
		focusLater(next >= 0 ? id(`s${step.key}-t${next}`) : id(`s${step.key}-pick`));
	}

	let busy = $state(false);
	const submit: SubmitFunction = () => {
		busy = true;
		return async ({ update, result }) => {
			try {
				await update({ reset: false });
				if (result.type === 'success' && result.data?.action === 'save') {
					steps = initialSteps();
					generation += 1;
				}
			} finally {
				busy = false;
			}
		};
	};
	// The delete asks with confirm() first, then sends `confirm=yes`. Without
	// JavaScript the action asks on the page (the panel below).
	const askDelete: SubmitFunction = (submission) => {
		if (!confirm(`Delete policy "${policy.name}"?`)) {
			submission.cancel();
			return;
		}
		submission.formData.set('confirm', 'yes');
		return submit(submission);
	};

	const message = (action: string) => (form?.action === action && 'message' in form ? form.message : null);
	const failed = (action: string) => form?.action === action && 'message' in form && !('saved' in form);
	const id = (name: string) => `policy-${name}`;
	const choice = 'flex items-center gap-2 text-sm text-ink-2';
</script>

<svelte:head>
	<title>{policy.name} · Escalation · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<div>
		<a href="/projects/{data.project.id}/escalation" class="link text-sm">← Escalation</a>
		<h2 class="mt-2 text-lg font-light wrap-anywhere text-ink">
			{policy.name}
			{#if policy.is_default}<span class="badge badge-primary ml-2 align-middle">Default</span>{/if}
		</h2>
	</div>

	{#if isAdmin}
		<Card title="Policy">
			{#if message('save') || message('default')}
				{@const action = message('save') ? 'save' : 'default'}
				<div class="alert {failed(action) ? 'alert-danger' : 'alert-success'} mb-4" role={failed(action) ? 'alert' : 'status'}>
					{message(action)}
				</div>
			{/if}
			{#key generation}
				<form method="POST" action="?/save" use:enhance={submit} class="space-y-5">
					<div class="grid gap-4 sm:grid-cols-3">
						<div>
							<label for={id('name')} class="label">Name</label>
							<input id={id('name')} name="name" type="text" required maxlength="200" defaultValue={data.editor.name} class="input" />
						</div>
						<div>
							<label for={id('repeat')} class="label">Repeat the steps</label>
							<input
								id={id('repeat')}
								name="repeat_count"
								type="number"
								required
								min="0"
								max={MAX_REPEAT_COUNT}
								step="1"
								defaultValue={data.editor.repeat_count}
								aria-describedby={id('repeat-hint')}
								class="input"
							/>
							<p id={id('repeat-hint')} class="hint">Times, 0 to {MAX_REPEAT_COUNT}, while nobody acknowledges.</p>
						</div>
						<div>
							<label for={id('fallback')} class="label">Fallback policy</label>
							<select id={id('fallback')} name="fallback_policy_id" class="select" aria-describedby={id('fallback-hint')}>
								<option value="" selected={data.editor.fallback_policy_id === ''}>None</option>
								{#each data.fallbacks as fallback (fallback.id)}
									<option value={fallback.id} selected={fallback.id === data.editor.fallback_policy_id}>{fallback.name}</option>
								{/each}
							</select>
							<p id={id('fallback-hint')} class="hint">Used when a pass notifies nobody.</p>
						</div>
					</div>

					<input type="hidden" name="step_count" value={steps.length} />
					<ol class="space-y-4">
						{#each steps as step, i (step.key)}
							<li class="space-y-3 rounded-control border border-border px-4 py-4">
								<div class="flex flex-wrap items-center gap-2">
									<h3 class="flex-1 text-sm font-medium text-ink">Step {i + 1}</h3>
									<button type="button" id={id(`s${step.key}-up`)} class="btn btn-ghost btn-sm" disabled={i === 0} onclick={() => move(i, -1)} aria-label="Move step {i + 1} up">↑</button>
									<button type="button" id={id(`s${step.key}-down`)} class="btn btn-ghost btn-sm" disabled={i === steps.length - 1} onclick={() => move(i, 1)} aria-label="Move step {i + 1} down">↓</button>
									<button type="button" id={id(`s${step.key}-remove`)} class="btn btn-ghost btn-sm" disabled={steps.length === 1} onclick={() => removeStep(i)}>
										Remove<span class="sr-only"> step {i + 1}</span>
									</button>
								</div>
								<div class="sm:w-1/3">
									<label for={id(`s${step.key}-delay`)} class="label">Delay (minutes)</label>
									<input
										id={id(`s${step.key}-delay`)}
										name="step_{i}_delay"
										type="number"
										required
										min="0"
										max={MAX_DELAY_MINUTES}
										step="1"
										bind:value={step.delay}
										class="input"
									/>
									<p class="hint">{i === 0 ? 'After the incident opens.' : 'After the previous step.'}</p>
								</div>

								<fieldset class="space-y-2">
									<legend class="label">Notify</legend>
									{#if step.targets.length === 0}
										<p class="text-sm text-danger-ink">Add at least one target.</p>
									{/if}
									<ul class="flex flex-wrap gap-2">
										{#each step.targets as target, t (target.value)}
											<li class="badge badge-primary">
												<input type="hidden" name="step_{i}_target" value={target.value} />
												<span class="wrap-anywhere">{target.label}</span>
												<button
													type="button"
													id={id(`s${step.key}-t${t}`)}
													class="ml-1 hover:text-ink"
													aria-label="Remove {target.label} from step {i + 1}"
													onclick={() => removeTarget(step, i, t)}>×</button
												>
											</li>
										{/each}
									</ul>
									{#if step.targets.length < MAX_TARGETS_PER_STEP}
										<div class="flex flex-wrap items-end gap-2">
											<div class="min-w-[14rem] flex-1">
												<label for={id(`s${step.key}-pick`)} class="sr-only">Target to add to step {i + 1}</label>
												<select id={id(`s${step.key}-pick`)} bind:value={step.pick} class="select">
													<option value="">Add a member, schedule or channel…</option>
													{#each data.choices.filter((c) => !step.targets.some((t) => t.value === c.value)) as option (option.value)}
														<option value={option.value}>{option.label}</option>
													{/each}
												</select>
											</div>
											<button type="button" class="btn btn-secondary btn-sm" disabled={step.pick === ''} onclick={() => addTarget(step, i)}>Add</button>
										</div>
									{/if}
								</fieldset>

								<label class={choice}>
									<input type="checkbox" name="step_{i}_condition" bind:checked={step.condition} />
									Only during these hours
								</label>
								{#if step.condition}
									<div class="space-y-3 border-l-2 border-border pl-4">
										<div class="flex flex-wrap gap-4">
											{#each WEEKDAYS as day (day.iso)}
												<label class={choice}>
													<input type="checkbox" name="step_{i}_weekday" value={day.iso} bind:group={step.weekdays} />
													{day.label}
												</label>
											{/each}
										</div>
										<div class="flex flex-wrap items-end gap-3">
											<div>
												<label for={id(`s${step.key}-from`)} class="label">From</label>
												<input id={id(`s${step.key}-from`)} name="step_{i}_from" type="time" required bind:value={step.from} class="input" />
											</div>
											<div>
												<label for={id(`s${step.key}-to`)} class="label">Until</label>
												<input id={id(`s${step.key}-to`)} name="step_{i}_to" type="time" required bind:value={step.to} class="input" />
											</div>
											<div class="min-w-[12rem] flex-1">
												<label for={id(`s${step.key}-zone`)} class="label">Time zone</label>
												<input id={id(`s${step.key}-zone`)} name="step_{i}_timezone" type="text" required list={id('zones')} bind:value={step.timezone} class="input" />
											</div>
										</div>
										<p class="hint">An end at or before the start runs past midnight. Outside these hours the step is skipped.</p>
									</div>
								{/if}
							</li>
						{/each}
					</ol>
					<p class="sr-only" role="status" aria-live="polite">{announcement}</p>
					<datalist id={id('zones')}>
						{#each zones as zone (zone)}
							<option value={zone}></option>
						{/each}
					</datalist>
					{#if steps.length < MAX_STEPS}
						<button type="button" class="btn btn-secondary btn-sm" onclick={addStep}>Add step</button>
					{/if}
					<div class="border-t border-border pt-4">
						<button type="submit" disabled={busy} class="btn btn-primary">Save policy</button>
					</div>
				</form>
			{/key}
		</Card>
	{:else}
		<Card title="Policy" subtitle="Only project admins can change escalation policies.">
			<dl class="grid gap-3 text-sm sm:grid-cols-2">
				<div><dt class="text-muted">Repeats</dt><dd class="text-ink">{policy.repeat_count === 0 ? 'Never' : `${policy.repeat_count} times`}</dd></div>
				<div><dt class="text-muted">Fallback policy</dt><dd class="wrap-anywhere text-ink">{fallbackName ?? 'None'}</dd></div>
			</dl>
			<ol class="mt-5 space-y-3">
				{#each policy.steps as step, i (step.position)}
					<li class="rounded-control border border-border px-4 py-3 text-sm">
						<p class="font-medium text-ink">Step {i + 1}: {after(step.delay_minutes, i)}</p>
						<p class="mt-1 wrap-anywhere text-ink-2">Notify {step.targets.map((t) => t.label).join(', ')}</p>
						{#if step.condition}
							<p class="mt-1 text-muted">Only {days(step.condition.weekdays)} {step.condition.from}–{step.condition.to} ({step.condition.timezone})</p>
						{/if}
					</li>
				{/each}
			</ol>
		</Card>
	{/if}

	{#if isAdmin}
		<Card title="Default and delete">
			{#if failed('delete')}
				<div class="alert alert-danger mb-4" role="alert">{message('delete')}</div>
			{/if}
			<div class="flex flex-wrap items-center gap-3">
				<p class="flex-1 text-sm text-muted">
					Sources without a policy of their own use the default. The default policy cannot be deleted.
				</p>
				{#if !policy.is_default}
					<form method="POST" action="?/makeDefault" use:enhance={submit}>
						<button type="submit" disabled={busy} class="btn btn-secondary btn-sm">Make default</button>
					</form>
				{/if}
				{#if form?.action === 'delete' && 'confirmDelete' in form}
					<form method="POST" action="?/delete" use:enhance={submit} class="alert alert-danger flex flex-wrap items-center gap-3">
						<input type="hidden" name="confirm" value="yes" />
						<span class="wrap-anywhere">Delete policy "{policy.name}"?</span>
						<button type="submit" disabled={busy} class="btn btn-danger-solid btn-sm">Yes, delete</button>
						<a href="?" class="text-ink-2 hover:text-ink">Cancel</a>
					</form>
				{:else}
					<form method="POST" action="?/delete" use:enhance={askDelete}>
						<button type="submit" disabled={busy} class="btn btn-danger btn-sm">Delete policy</button>
					</form>
				{/if}
			</div>
		</Card>
	{/if}
</div>
