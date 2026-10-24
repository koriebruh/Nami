<script lang="ts">
	// The ingest pipeline editor: an ordered list of rule cards, the hidden
	// `rules` field the save form posts, and a live preview over sample events
	// (the engine runs the unsaved rules; nothing is stored). Everything renders
	// as text, never as HTML.
	import { goto } from '$app/navigation';
	import { untrack } from 'svelte';
	import {
		describeRule,
		draftFromRule,
		emptyDraft,
		MAX_REGEXES,
		MAX_RULES,
		parseSampleEvents,
		regexCount,
		RULE_KINDS,
		ruleFromDraft,
		usesMatch,
		type Rule,
		type RuleDraft,
		type TestResult
	} from '#lib/logs/pipeline';

	let {
		rules,
		previewUrl,
		valid = $bindable(false)
	}: { rules: Rule[]; previewUrl: string; valid?: boolean } = $props();

	type KeyedDraft = RuleDraft & { key: number };
	let nextKey = 0;
	const keyed = (draft: RuleDraft): KeyedDraft => ({ ...draft, key: nextKey++ });

	// The editor owns its drafts from here on: a reload of the page data after
	// a save must not reset what is being edited.
	let drafts = $state<KeyedDraft[]>(untrack(() => rules).map((rule) => keyed(draftFromRule(rule))));

	const SAMPLE = JSON.stringify(
		[
			{ level: 'info', message: 'GET /healthz 200', status: 200 },
			{ level: 'error', message: 'payment failed', user: { email: 'ana@example.com' }, password: 'hunter2' }
		],
		null,
		2
	);
	let samples = $state(SAMPLE);

	const checks = $derived(drafts.map((draft) => ruleFromDraft(draft)));
	/** Every rule, or null while any draft is invalid. */
	const validRules = $derived.by(() => {
		const out: Rule[] = [];
		for (const check of checks) {
			if (!check.ok) return null;
			out.push(check.rule);
		}
		return out;
	});
	const regexes = $derived(regexCount(checks.flatMap((check) => (check.ok ? [check.rule] : []))));
	const listError = $derived(
		regexes > MAX_REGEXES ? `A pipeline may hold at most ${MAX_REGEXES} regular expressions (this one has ${regexes}).` : null
	);
	const sampleCheck = $derived(parseSampleEvents(samples));

	$effect(() => {
		valid = validRules !== null && listError === null;
	});

	function add() {
		if (drafts.length < MAX_RULES) drafts.push(keyed(emptyDraft('drop')));
	}
	function remove(i: number) {
		drafts.splice(i, 1);
	}
	function move(i: number, by: -1 | 1) {
		const j = i + by;
		if (j < 0 || j >= drafts.length) return;
		[drafts[i], drafts[j]] = [drafts[j], drafts[i]];
	}
	/** Keeps what was typed; only fixes what the new kind cannot take. */
	function kindChanged(draft: KeyedDraft) {
		if (draft.type === 'drop' && draft.matchMode === 'none') draft.matchMode = 'field';
		if (draft.type === 'sample' && draft.n === '') draft.n = '10';
	}

	// Live preview, 500 ms after the last change; a newer one aborts the last.
	let results = $state<TestResult[] | null>(null);
	let previewError = $state<string | null>(null);
	let previewing = $state(false);

	/** The engine names rules and events by index from 0; the page counts from 1. */
	function readable(message: string): string {
		return message
			.replace(/rules\[(\d+)\]/g, (_, i) => `Rule ${Number(i) + 1}`)
			.replace(/events\[(\d+)\]/g, (_, i) => `Sample event ${Number(i) + 1}`);
	}

	const payload = $derived(
		validRules !== null && listError === null && sampleCheck.ok
			? JSON.stringify({ rules: validRules, events: sampleCheck.events })
			: null
	);

	$effect(() => {
		const body = payload;
		if (body === null) return;
		const controller = new AbortController();
		const timer = setTimeout(async () => {
			previewing = true;
			try {
				const response = await fetch(previewUrl, {
					method: 'POST',
					headers: { 'content-type': 'application/json' },
					body,
					signal: controller.signal
				});
				if (response.status === 401) {
					await goto('/login');
					return;
				}
				const answer = await response.json().catch(() => null);
				if (response.status === 403) {
					previewError = 'Only project members and admins can run the preview.';
					results = null;
					return;
				}
				if (!response.ok) {
					previewError = readable(
						typeof answer?.error === 'string' ? answer.error : `The preview failed (HTTP ${response.status}).`
					);
					results = null;
					return;
				}
				previewError = null;
				results = Array.isArray(answer?.results) ? answer.results : [];
			} catch (cause) {
				if ((cause as Error)?.name === 'AbortError') return;
				previewError = 'The preview could not reach the server.';
				results = null;
			} finally {
				if (!controller.signal.aborted) previewing = false;
			}
		}, 500);
		return () => {
			clearTimeout(timer);
			controller.abort();
			previewing = false;
		};
	});

	const OUTCOME: Record<TestResult['outcome'], { label: string; badge: string }> = {
		kept: { label: 'kept', badge: 'badge-success' },
		dropped: { label: 'dropped', badge: 'badge-danger' },
		sampled_out: { label: 'sampled out', badge: 'badge-neutral' }
	};

	const button = 'btn btn-ghost btn-sm';
</script>

<input type="hidden" name="rules" value={JSON.stringify(validRules ?? [])} />

<div class="grid gap-6 lg:grid-cols-2">
	<section class="space-y-3" aria-labelledby="pipeline-rules-title">
		<h3 id="pipeline-rules-title" class="card-title">Rules</h3>
		{#if drafts.length === 0}
			<p class="text-sm text-muted">No rules: events are stored as they arrive.</p>
		{/if}
		<ol class="space-y-3">
			{#each drafts as draft, i (draft.key)}
				{@const check = checks[i]}
				{@const id = `rule-${draft.key}`}
				<li class="card space-y-3 p-4">
					<div class="flex flex-wrap items-center justify-between gap-2">
						<div class="min-w-0">
							<span class="text-sm font-medium text-ink">Rule {i + 1}</span>
							{#if check?.ok}
								<span class="ml-2 text-xs wrap-anywhere text-muted">{describeRule(check.rule)}</span>
							{/if}
						</div>
						<div class="flex gap-1">
							<button type="button" class={button} disabled={i === 0} onclick={() => move(i, -1)} aria-label="Move rule {i + 1} up"
								>↑</button
							>
							<button
								type="button"
								class={button}
								disabled={i === drafts.length - 1}
								onclick={() => move(i, 1)}
								aria-label="Move rule {i + 1} down">↓</button
							>
							<button type="button" class="btn btn-danger btn-sm" onclick={() => remove(i)} aria-label="Remove rule {i + 1}"
								>Remove</button
							>
						</div>
					</div>

					<div class="grid gap-3 sm:grid-cols-2">
						<div>
							<label class="label" for="{id}-type">Kind</label>
							<select id="{id}-type" class="select" bind:value={draft.type} onchange={() => kindChanged(draft)}>
								{#each RULE_KINDS as kind (kind.value)}
									<option value={kind.value}>{kind.label}</option>
								{/each}
							</select>
						</div>

						{#if draft.type === 'remove_fields'}
							<div class="sm:col-span-2">
								<label class="label" for="{id}-paths">Field paths</label>
								<input id="{id}-paths" class="input" bind:value={draft.paths} placeholder="user.email, password" />
								<p class="hint">Comma-separated. Level and message cannot be removed.</p>
							</div>
						{:else if draft.type === 'redact'}
							<div class="sm:col-span-2">
								<label class="label" for="{id}-keys">Keys</label>
								<input id="{id}-keys" class="input" bind:value={draft.keys} placeholder="password, token" />
								<p class="hint">Comma-separated, case-insensitive, at any depth.</p>
							</div>
							<div class="sm:col-span-2">
								<label class="label" for="{id}-regex">Regex</label>
								<input id="{id}-regex" class="input font-mono" bind:value={draft.regex} placeholder="sk_[a-z0-9]+" />
								<p class="hint">Applied to the message and every string or number value of the fields.</p>
							</div>
						{:else if draft.type === 'rename'}
							<div>
								<label class="label" for="{id}-from">From</label>
								<input id="{id}-from" class="input" bind:value={draft.from} placeholder="msg" />
							</div>
							<div>
								<label class="label" for="{id}-to">To</label>
								<input id="{id}-to" class="input" bind:value={draft.to} placeholder="message" />
							</div>
						{:else if draft.type === 'sample'}
							<div>
								<label class="label" for="{id}-n">Keep 1 in N</label>
								<input id="{id}-n" class="input" inputmode="numeric" bind:value={draft.n} />
							</div>
						{/if}

						{#if usesMatch(draft.type)}
							<div class="sm:col-span-2">
								<label class="label" for="{id}-match">When</label>
								<select id="{id}-match" class="select" bind:value={draft.matchMode}>
									{#if draft.type === 'sample'}<option value="none">Every event</option>{/if}
									<option value="field">A field equals a value</option>
									<option value="regex">The message matches a regex</option>
								</select>
							</div>
							{#if draft.matchMode === 'field'}
								<div>
									<label class="label" for="{id}-field">Field</label>
									<input id="{id}-field" class="input" bind:value={draft.field} placeholder="level" />
								</div>
								<div>
									<label class="label" for="{id}-equals">Equals</label>
									<input id="{id}-equals" class="input" bind:value={draft.equals} placeholder="debug" />
								</div>
							{:else if draft.matchMode === 'regex'}
								<div class="sm:col-span-2">
									<label class="label" for="{id}-message-regex">Message regex</label>
									<input id="{id}-message-regex" class="input font-mono" bind:value={draft.messageRegex} placeholder="^healthz" />
								</div>
							{/if}
						{/if}
					</div>

					{#if check && !check.ok}
						<p class="text-xs text-danger-ink" role="alert">{check.message}</p>
					{/if}
				</li>
			{/each}
		</ol>
		<div class="flex flex-wrap items-center gap-3">
			<button type="button" class="btn btn-secondary btn-sm" disabled={drafts.length >= MAX_RULES} onclick={add}>Add rule</button>
			<span class="text-xs text-muted tabular-nums">{drafts.length}/{MAX_RULES} rules · {regexes}/{MAX_REGEXES} regexes</span>
		</div>
		{#if listError}
			<div role="alert" class="alert alert-danger">{listError}</div>
		{/if}
	</section>

	<section class="space-y-3" aria-labelledby="pipeline-preview-title">
		<h3 id="pipeline-preview-title" class="card-title">Preview</h3>
		<div>
			<label class="label" for="pipeline-samples">Sample events</label>
			<textarea id="pipeline-samples" class="input h-48 font-mono text-xs" bind:value={samples} spellcheck="false"></textarea>
			<p class="hint">
				A JSON array, one object, or one object per line; at most 100. The preview runs the rules above without saving
				them, and sample counters restart on every preview.
			</p>
		</div>
		{#if !sampleCheck.ok}
			<div role="alert" class="alert alert-warning">{sampleCheck.message}</div>
		{:else if validRules === null || listError}
			<p class="text-sm text-muted">Fix the rules to see a preview.</p>
		{/if}
		{#if previewError}
			<div role="alert" class="alert alert-danger">{previewError}</div>
		{/if}
		<div aria-live="polite" aria-busy={previewing} class="space-y-2">
			{#if results && payload !== null}
				{#each results as result, i (i)}
					<div class="rounded-control border border-border bg-surface-2 p-3">
						<div class="flex items-center gap-2 text-xs">
							<span class="text-muted">Event {i + 1}</span>
							<span class="badge {OUTCOME[result.outcome]?.badge ?? 'badge-neutral'}"
								>{OUTCOME[result.outcome]?.label ?? result.outcome}</span
							>
						</div>
						{#if result.outcome === 'kept'}
							<pre class="mt-2 font-mono text-xs break-all whitespace-pre-wrap text-ink-2">{JSON.stringify(
									result.event,
									null,
									2
								)}</pre>
						{/if}
					</div>
				{/each}
			{/if}
		</div>
	</section>
</div>
