<script lang="ts">
	// The log alert editor: condition, schedule and the query it counts, plus a
	// live count of what the unsaved query matches now (the engine runs it;
	// nothing is stored). It renders inside the page's <form>; every value is
	// posted as a plain field and parsed by the BFF.
	import { goto } from '$app/navigation';
	import { untrack } from 'svelte';
	import {
		conditionMet,
		formatSeconds,
		INTERVAL_PRESETS,
		WINDOW_PRESETS,
		type LogAlertQuery,
		type LogAlertSettings
	} from '#lib/logs/log-alerts';
	import {
		FILTER_OPS,
		filterNeedsValue,
		isFieldPath,
		LEVELS,
		MAX_FILTERS,
		type FieldFilter,
		type FilterOp,
		type LogLevel,
		type Source
	} from '#lib/logs/logs';

	let {
		initial,
		sources,
		previewUrl,
		submitLabel,
		busy = false
	}: { initial: LogAlertSettings; sources: Source[]; previewUrl: string; submitLabel: string; busy?: boolean } = $props();

	// The form owns its values from here on: a reload of the page data after a
	// save must not reset what is being edited.
	const start = untrack(() => initial);
	let name = $state(start.name);
	let comparison = $state(start.comparison);
	let threshold = $state<number | null>(start.threshold);
	let windowSeconds = $state(start.window_seconds);
	let intervalSeconds = $state(start.interval_seconds);
	let enabled = $state(start.enabled);
	let q = $state(start.query.q ?? '');
	let inFields = $state(start.query.in_fields ?? false);
	let levels = $state<LogLevel[]>([...(start.query.levels ?? [])]);
	let sourceIds = $state<string[]>([...(start.query.source_ids ?? [])]);
	let patterns = $state<string[]>([...(start.query.pattern_hashes ?? [])]);

	type FilterRow = { key: number; path: string; op: FilterOp; value: string };
	let nextKey = 0;
	let filters = $state<FilterRow[]>(
		(start.query.filters ?? []).map((f) => ({ key: nextKey++, path: f.path, op: f.op, value: f.value ?? '' }))
	);

	// A preset list plus the stored value when it is not one of them.
	const withCurrent = (presets: { seconds: number; label: string }[], current: number) =>
		presets.some((p) => p.seconds === current) ? presets : [...presets, { seconds: current, label: formatSeconds(current) }].sort((a, b) => a.seconds - b.seconds);
	const windowOptions = withCurrent(WINDOW_PRESETS, start.window_seconds);
	const intervalOptions = withCurrent(INTERVAL_PRESETS, start.interval_seconds);

	// A source the alert names that is gone: still listed, so it can be unticked.
	const knownSources = $derived([
		...sources.map((s) => ({ id: s.id, name: s.name })),
		...sourceIds.filter((id) => !sources.some((s) => s.id === id)).map((id) => ({ id, name: `Deleted source (${id.slice(0, 8)})` }))
	]);

	const rowFilter = (row: FilterRow): FieldFilter =>
		filterNeedsValue(row.op) ? { path: row.path.trim(), op: row.op, value: row.value } : { path: row.path.trim(), op: row.op };
	const filledRows = $derived(filters.filter((row) => row.path.trim() !== ''));
	const badFilter = $derived(filledRows.some((row) => !isFieldPath(row.path.trim())));

	/** The query as the engine takes it; null while a filter path is invalid. */
	const query = $derived.by((): LogAlertQuery | null => {
		if (badFilter) return null;
		const out: LogAlertQuery = {};
		if (q.trim()) out.q = q.trim();
		if (inFields) out.in_fields = true;
		if (levels.length) out.levels = levels;
		if (sourceIds.length) out.source_ids = sourceIds;
		if (filledRows.length) out.filters = filledRows.map(rowFilter);
		if (patterns.length) out.pattern_hashes = patterns;
		return out;
	});

	function addFilter() {
		if (filters.length < MAX_FILTERS) filters.push({ key: nextKey++, path: '', op: 'eq', value: '' });
	}

	// Live count, 500 ms after the last change; a newer one aborts the last.
	let count = $state<number | null>(null);
	let previewError = $state<string | null>(null);
	let previewing = $state(false);
	let retry = $state(0);

	const payload = $derived(query === null ? null : JSON.stringify({ query, window_seconds: windowSeconds }));

	$effect(() => {
		const body = payload;
		void retry;
		if (body === null) return;
		const controller = new AbortController();
		let retryTimer: ReturnType<typeof setTimeout> | undefined;
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
				if (response.status === 429) {
					previewError = 'Too many queries; the preview will retry.';
					retryTimer = setTimeout(() => (retry += 1), 3000);
					return;
				}
				if (!response.ok || typeof answer?.count !== 'number') {
					previewError =
						typeof answer?.error === 'string' ? answer.error : `The preview failed (HTTP ${response.status}).`;
					count = null;
					return;
				}
				previewError = null;
				count = answer.count;
			} catch (cause) {
				if ((cause as Error)?.name === 'AbortError') return;
				previewError = 'The preview could not reach the server.';
				count = null;
			} finally {
				if (!controller.signal.aborted) previewing = false;
			}
		}, 500);
		return () => {
			clearTimeout(timer);
			clearTimeout(retryTimer);
			controller.abort();
			previewing = false;
		};
	});

	const thresholdNumber = $derived(
		typeof threshold === 'number' && Number.isInteger(threshold) && threshold >= 0 ? threshold : null
	);
	const wouldTrigger = $derived(
		count !== null && thresholdNumber !== null && conditionMet({ comparison, threshold: thresholdNumber }, count)
	);

	const input = 'input';
	const label = 'label';
	const select = 'select';
	const levelLabel = (level: string) => level[0].toUpperCase() + level.slice(1);
</script>

<div class="space-y-6">
	<div class="grid gap-4 sm:grid-cols-2">
		<div class="sm:col-span-2">
			<label for="alert-name" class={label}>Name</label>
			<input id="alert-name" name="name" type="text" required maxlength="200" bind:value={name} class={input} />
		</div>
		<div>
			<label for="alert-comparison" class={label}>Alert when</label>
			<select id="alert-comparison" name="comparison" bind:value={comparison} class={select}>
				<option value="gt">more than</option>
				<option value="lt">fewer than</option>
			</select>
		</div>
		<div>
			<label for="alert-threshold" class={label}>Threshold</label>
			<input
				id="alert-threshold"
				name="threshold"
				type="number"
				required
				min={comparison === 'lt' ? 1 : 0}
				max="1000000000000"
				step="1"
				bind:value={threshold}
				class={input}
			/>
		</div>
		<div>
			<label for="alert-window" class={label}>matching events in</label>
			<select id="alert-window" name="window_seconds" bind:value={windowSeconds} class={select}>
				{#each windowOptions as option (option.seconds)}
					<option value={option.seconds}>{option.label}</option>
				{/each}
			</select>
		</div>
		<div>
			<label for="alert-interval" class={label}>Check every</label>
			<select id="alert-interval" name="interval_seconds" bind:value={intervalSeconds} class={select}>
				{#each intervalOptions as option (option.seconds)}
					<option value={option.seconds}>{option.label}</option>
				{/each}
			</select>
		</div>
		<label class="flex items-center gap-2 text-sm text-ink sm:col-span-2">
			<input type="checkbox" name="enabled" bind:checked={enabled} class="checkbox" />
			Enabled
		</label>
	</div>

	<fieldset class="space-y-4 border-t border-border pt-4">
		<legend class="card-title">Matching events</legend>
		<div>
			<label for="alert-q" class={label}>Search text</label>
			<input id="alert-q" name="q" type="search" maxlength="1024" bind:value={q} class={input} />
			<label class="mt-2 flex items-center gap-2 text-sm text-ink-2">
				<input type="checkbox" name="in_fields" bind:checked={inFields} class="checkbox" />
				Also search fields
			</label>
		</div>

		<fieldset>
			<legend class={label}>Levels</legend>
			<div class="flex flex-wrap gap-x-4 gap-y-2">
				{#each LEVELS as level (level)}
					<label class="flex items-center gap-2 text-sm text-ink-2">
						<input type="checkbox" name="level" value={level} bind:group={levels} class="checkbox" />
						{levelLabel(level)}
					</label>
				{/each}
			</div>
			<p class="hint">None ticked: every level.</p>
		</fieldset>

		<fieldset>
			<legend class={label}>Sources</legend>
			{#if knownSources.length === 0}
				<p class="text-sm text-muted">No sources yet.</p>
			{:else}
				<div class="flex flex-wrap gap-x-4 gap-y-2">
					{#each knownSources as source (source.id)}
						<label class="flex items-center gap-2 text-sm wrap-anywhere text-ink-2">
							<input type="checkbox" name="source" value={source.id} bind:group={sourceIds} class="checkbox" />
							{source.name}
						</label>
					{/each}
				</div>
				<p class="hint">None ticked: every source.</p>
			{/if}
		</fieldset>

		<fieldset class="space-y-2">
			<legend class={label}>Field filters</legend>
			{#each filters as row, i (row.key)}
				<div class="flex flex-wrap items-center gap-2">
					<input
						type="text"
						aria-label="Filter {i + 1} field path"
						placeholder="http.status"
						maxlength="330"
						bind:value={row.path}
						class="{input} w-auto min-w-0 flex-1"
					/>
					<select aria-label="Filter {i + 1} operator" bind:value={row.op} class="{select} w-auto">
						{#each FILTER_OPS as op (op.value)}
							<option value={op.value}>{op.label}</option>
						{/each}
					</select>
					{#if filterNeedsValue(row.op)}
						<input
							type="text"
							aria-label="Filter {i + 1} value"
							maxlength="1024"
							bind:value={row.value}
							class="{input} w-auto min-w-0 flex-1"
						/>
					{/if}
					<button type="button" class="btn btn-ghost btn-sm" onclick={() => filters.splice(i, 1)}>
						Remove<span class="sr-only"> filter {i + 1}</span>
					</button>
				</div>
			{/each}
			{#each filledRows as row (row.key)}
				<input type="hidden" name="filter" value={JSON.stringify(rowFilter(row))} />
			{/each}
			{#if badFilter}
				<p class="text-sm text-danger-ink" role="alert">
					A field path is 1-5 dot-separated parts of letters, digits, _ and -.
				</p>
			{/if}
			<button type="button" class="btn btn-secondary btn-sm" disabled={filters.length >= MAX_FILTERS} onclick={addFilter}>
				Add filter
			</button>
		</fieldset>

		{#if patterns.length > 0}
			<fieldset>
				<legend class={label}>Patterns</legend>
				<div class="flex flex-wrap gap-2">
					{#each patterns as pattern (pattern)}
						<span class="badge badge-primary">
							<input type="hidden" name="pattern" value={pattern} />
							<span class="font-mono">#{pattern}</span>
							<button
								type="button"
								class="ml-1 hover:text-ink"
								aria-label="Remove pattern {pattern}"
								onclick={() => (patterns = patterns.filter((p) => p !== pattern))}>×</button
							>
						</span>
					{/each}
				</div>
			</fieldset>
		{/if}
	</fieldset>

	<div class="rounded-control border border-border bg-surface-2 px-4 py-3 text-sm" aria-live="polite">
		{#if query === null}
			<span class="text-muted">Fix the field filters to see the count.</span>
		{:else if previewError}
			<span class="text-danger-ink">{previewError}</span>
		{:else if count === null}
			<span class="text-muted">Counting matching events…</span>
		{:else}
			<span class="text-ink">
				<span class="font-medium tabular-nums">{count.toLocaleString('en-US')}</span>
				matching {count === 1 ? 'event' : 'events'} in the last {formatSeconds(windowSeconds)}{previewing ? ' (updating…)' : ''}.
			</span>
			{#if wouldTrigger}
				<span class="badge badge-danger ml-2">would trigger now</span>
			{/if}
		{/if}
	</div>

	<div class="border-t border-border pt-4">
		<button type="submit" class="btn btn-primary" disabled={busy || query === null}>{submitLabel}</button>
	</div>
</div>
