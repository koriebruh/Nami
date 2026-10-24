<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { SvelteMap, SvelteSet } from 'svelte/reactivity';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import LogVolumeChart from '#lib/components/LogVolumeChart.svelte';
	import {
		engineParams,
		entryHref,
		explorerHref,
		fieldAt,
		FILTER_OPS,
		filterNeedsValue,
		formatFieldValue,
		formatLogTime,
		fromInputValue,
		isFieldPath,
		LEVELS,
		levelClasses,
		MAX_COLUMNS,
		MAX_FILTERS,
		PAGE_SIZE,
		tailKey,
		TIME_PRESETS,
		toggled,
		toInputValue,
		type ExplorerState,
		type FieldFilter,
		type FilterOp,
		type LogEntry
	} from '#lib/logs/logs';
	import { startLiveTail, type LiveStatus } from '#lib/logs/live-tail';
	import { formatUtc } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	// Log text is attacker-controlled (anyone with a source token can send
	// it): everything below renders as text, never as HTML, and nothing is
	// turned into a link.

	const path = $derived(page.url.pathname);
	const sourceNames = $derived(new Map(data.sources.map((source) => [source.id, source.name])));
	const opLabels = new Map(FILTER_OPS.map((op) => [op.value, op.label]));
	const MAX_WINDOW_MS = 31 * 24 * 3600 * 1000;

	/** Every change is a navigation: the URL is the explorer's state. */
	function go(change: Partial<ExplorerState>) {
		void goto(explorerHref(path, { ...data.state, ...change }), { reset: false });
	}

	// Results: the loaded page, extended by "Load more"; both reset on navigation.
	let rows = $derived(data.logs);
	let nextCursor = $derived(data.nextCursor);
	let loadingMore = $state(false);
	let loadError = $state<string | null>(null);
	const expanded = new SvelteSet<string>();

	// "Load more" extends the page it was clicked on: a navigation aborts it,
	// and an answer for an older page is dropped. Plain variables on purpose:
	// the effect below must not depend on them.
	let generation = 0;
	let controller: AbortController | null = null;
	$effect(() => {
		void data.logs;
		generation += 1;
		controller?.abort();
		controller = null;
		loadingMore = false;
		loadError = null;
	});

	async function loadMore() {
		if (!nextCursor || loadingMore) return;
		// The generation alone could miss a navigation whose effect has not
		// run yet; the page's own rows cannot.
		const mine = generation;
		const base = data.logs;
		const stale = () => mine !== generation || data.logs !== base;
		const own = new AbortController();
		controller = own;
		loadingMore = true;
		loadError = null;
		const params = engineParams(data.state, data.window);
		params.set('cursor', nextCursor);
		params.set('limit', String(PAGE_SIZE));
		try {
			const response = await fetch(`${path}/search?${params}`, { signal: own.signal });
			if (stale()) return;
			if (response.status === 401) {
				await goto('/login');
				return;
			}
			const body = await response.json().catch(() => null);
			if (stale()) return;
			if (response.ok && body) {
				rows = [...rows, ...body.logs];
				nextCursor = body.next_cursor;
			} else {
				loadError = body?.error ?? 'Could not load more logs.';
			}
		} catch {
			// An abort only comes from a navigation: nothing to report then.
			if (!stale()) loadError = 'Could not load more logs.';
		} finally {
			if (!stale()) {
				loadingMore = false;
				controller = null;
			}
		}
	}

	// Live tail: Server-Sent Events through the BFF while `live` is on (see
	// `startLiveTail`). It restarts only when the filters it sends change
	// (not for columns or the time range), and stops when the toggle goes
	// off, which also clears its rows.
	const liveKey = $derived(data.state.live ? tailKey(data.state) : null);
	let liveRows = $state<LogEntry[]>([]);
	let liveStatus = $state<'off' | LiveStatus>('off');
	let liveNotice = $state<string | null>(null);
	$effect(() => {
		liveRows = [];
		liveNotice = null;
		if (liveKey === null) {
			liveStatus = 'off';
			return;
		}
		return startLiveTail(`${path}/tail${liveKey ? `?${liveKey}` : ''}`, {
			onStatus: (status, notice) => {
				liveStatus = status;
				liveNotice = notice;
			},
			onRows: (next) => {
				liveRows = next;
			}
		});
	});

	const shown = $derived.by(() => {
		const seen = new Set<string>();
		return [...liveRows, ...rows].filter((row) => {
			if (seen.has(row.id)) return false;
			seen.add(row.id);
			return true;
		});
	});

	// Toolbar forms
	let showCustom = $state(false);
	let rangeError = $state<string | null>(null);
	let filterOp = $state<FilterOp>('eq');
	let filterError = $state<string | null>(null);
	let columnError = $state<string | null>(null);

	function search(event: SubmitEvent) {
		event.preventDefault();
		const form = new FormData(event.currentTarget as HTMLFormElement);
		go({ q: String(form.get('q') ?? '').trim(), inFields: form.get('in_fields') === 'on' });
	}

	function applyCustom(event: SubmitEvent) {
		event.preventDefault();
		const form = new FormData(event.currentTarget as HTMLFormElement);
		const from = fromInputValue(String(form.get('from') ?? ''));
		const to = fromInputValue(String(form.get('to') ?? ''));
		if (!from || !to || from >= to) {
			rangeError = 'Pick a start before the end.';
			return;
		}
		if (Date.parse(to) - Date.parse(from) > MAX_WINDOW_MS) {
			rangeError = 'The time range may span at most 31 days.';
			return;
		}
		rangeError = null;
		go({ range: 'custom', from, to });
	}

	function addFilter(event: SubmitEvent) {
		event.preventDefault();
		const form = new FormData(event.currentTarget as HTMLFormElement);
		const filterPath = String(form.get('path') ?? '').trim();
		const value = String(form.get('value') ?? '');
		if (!isFieldPath(filterPath)) {
			filterError = 'A field path is 1-5 dot-separated names of letters, digits, _ or -.';
			return;
		}
		if (data.state.filters.length >= MAX_FILTERS) {
			filterError = `At most ${MAX_FILTERS} filters.`;
			return;
		}
		if (filterNeedsValue(filterOp) && value === '') {
			filterError = 'Enter a value to compare with.';
			return;
		}
		filterError = null;
		const filter: FieldFilter = filterNeedsValue(filterOp)
			? { path: filterPath, op: filterOp, value }
			: { path: filterPath, op: filterOp };
		go({ filters: [...data.state.filters, filter] });
	}

	function addColumn(event: SubmitEvent) {
		event.preventDefault();
		const form = new FormData(event.currentTarget as HTMLFormElement);
		const column = String(form.get('column') ?? '').trim();
		if (!isFieldPath(column)) {
			columnError = 'A field path is 1-5 dot-separated names of letters, digits, _ or -.';
			return;
		}
		if (data.state.columns.includes(column)) {
			columnError = 'That column is already shown.';
			return;
		}
		if (data.state.columns.length >= MAX_COLUMNS) {
			columnError = `At most ${MAX_COLUMNS} columns.`;
			return;
		}
		columnError = null;
		go({ columns: [...data.state.columns, column] });
	}

	// List rows may be cut by the engine (long message, large fields): an
	// expanded one loads the whole row once, by id and dt.
	const fullRows = new SvelteMap<string, LogEntry>();
	const fullErrors = new SvelteMap<string, string>();
	const fullLoading = new SvelteSet<string>();

	async function loadFull(row: LogEntry) {
		if (fullRows.has(row.id) || fullLoading.has(row.id)) return;
		fullLoading.add(row.id);
		fullErrors.delete(row.id);
		try {
			const response = await fetch(entryHref(path, row));
			if (response.status === 401) {
				await goto('/login');
				return;
			}
			const body = await response.json().catch(() => null);
			if (response.ok && body) fullRows.set(row.id, body);
			else fullErrors.set(row.id, body?.error ?? 'Could not load the whole entry.');
		} catch {
			fullErrors.set(row.id, 'Could not load the whole entry.');
		} finally {
			fullLoading.delete(row.id);
		}
	}

	function toggleRow(row: LogEntry) {
		if (expanded.has(row.id)) {
			expanded.delete(row.id);
			return;
		}
		expanded.add(row.id);
		if (row.truncated) void loadFull(row);
	}

	const input = 'input w-auto py-1.5';
	const button = 'btn btn-secondary';
	const chip = (on: boolean) =>
		`rounded-control border px-2.5 py-1 text-xs transition-colors ${on ? 'border-primary text-primary' : 'border-transparent text-ink-2 hover:text-primary'}`;
</script>

<svelte:head>
	<title>Explorer · Logs · {data.project.name}</title>
</svelte:head>

<div class="space-y-4">
	<div class="card space-y-3 p-4">
		{#key `${data.state.q}|${data.state.inFields}`}
			<form class="flex flex-wrap items-center gap-2" onsubmit={search}>
				<label class="sr-only" for="log-q">Search</label>
				<input
					id="log-q"
					name="q"
					type="search"
					maxlength="1024"
					placeholder="Search messages (case-insensitive)"
					defaultValue={data.state.q}
					class="{input} min-w-64 flex-1"
				/>
				<label class="flex items-center gap-2 text-sm text-ink-2">
					<input type="checkbox" name="in_fields" defaultChecked={data.state.inFields} /> Also search fields
				</label>
				<button type="submit" class="btn btn-primary">Search</button>
			</form>
		{/key}

		<div class="flex flex-wrap items-center justify-between gap-3">
			<div class="flex flex-wrap items-center gap-1" role="group" aria-label="Time range">
				{#each TIME_PRESETS as preset (preset.value)}
					<button
						type="button"
						aria-pressed={data.state.range === preset.value}
						onclick={() => go({ range: preset.value, from: null, to: null })}
						class={chip(data.state.range === preset.value)}>{preset.label}</button
					>
				{/each}
				<button
					type="button"
					aria-pressed={data.state.range === 'custom'}
					onclick={() => (showCustom = !showCustom)}
					class={chip(data.state.range === 'custom')}>Custom</button
				>
			</div>
			<div class="flex flex-wrap items-center gap-2">
				<details class="relative">
					<summary class="{button} cursor-pointer list-none">
						{data.state.sources.length === 0
							? 'All sources'
							: data.state.sources.length === 1
								? (sourceNames.get(data.state.sources[0]) ?? '1 source')
								: `${data.state.sources.length} sources`}
					</summary>
					<div class="absolute right-0 z-10 mt-1 w-64 space-y-1 rounded-control border border-border bg-surface p-2 shadow-pop">
						{#each data.sources as source (source.id)}
							<label class="flex items-center gap-2 rounded px-2 py-1 text-sm text-ink-2 hover:bg-primary-soft">
								<input
									type="checkbox"
									checked={data.state.sources.includes(source.id)}
									onchange={() => go({ sources: toggled(data.state.sources, source.id) })}
								/>
								{source.name}
							</label>
						{:else}
							{#if data.sourcesError}
								<p class="px-2 py-1 text-sm text-danger-ink" role="alert">{data.sourcesError}</p>
							{:else}
								<p class="px-2 py-1 text-sm text-muted">
									No sources yet — <a href="{path}/sources" class="text-primary">create one</a>.
								</p>
							{/if}
						{/each}
					</div>
				</details>
				<button
					type="button"
					aria-pressed={data.state.live}
					onclick={() => go({ live: !data.state.live })}
					class="{button} {data.state.live ? 'border-primary/40 text-primary' : ''}"
				>
					{data.state.live ? 'Stop live tail' : 'Live tail'}
				</button>
			</div>
		</div>

		{#if showCustom || data.state.range === 'custom'}
			<form class="flex flex-wrap items-center gap-2 text-sm text-ink-2" onsubmit={applyCustom}>
				<label for="log-from">From</label>
				<input id="log-from" name="from" type="datetime-local" defaultValue={toInputValue(data.window.from)} class={input} />
				<label for="log-to">to</label>
				<input id="log-to" name="to" type="datetime-local" defaultValue={toInputValue(data.window.to)} class={input} />
				<span class="text-xs text-muted">UTC</span>
				<button type="submit" class={button}>Apply</button>
				{#if rangeError}<span class="text-danger-ink" role="alert">{rangeError}</span>{/if}
			</form>
		{/if}

		<div class="flex flex-wrap items-center gap-1" role="group" aria-label="Levels">
			{#each LEVELS as level (level)}
				{@const on = data.state.levels.includes(level)}
				<button
					type="button"
					aria-pressed={on}
					onclick={() => go({ levels: toggled(data.state.levels, level) })}
					class="rounded-control px-2 py-0.5 text-xs transition-colors {on
						? `${levelClasses(level)} ring-1 ring-current`
						: 'text-muted hover:text-ink-2'}">{level}</button
				>
			{/each}
		</div>

		<div class="flex flex-wrap items-center gap-2">
			{#each data.state.filters as filter, index (index)}
				<span class="flex items-center gap-1 rounded-control border border-border bg-surface-2 px-2 py-0.5 text-xs text-ink-2">
					<code>{filter.path}</code>
					<span class="text-muted">{opLabels.get(filter.op)}</span>
					{#if filter.value !== undefined}<code class="text-primary">{filter.value}</code>{/if}
					<button
						type="button"
						aria-label="Remove filter on {filter.path}"
						onclick={() => go({ filters: data.state.filters.filter((_, other) => other !== index) })}
						class="text-muted hover:text-danger-ink">×</button
					>
				</span>
			{/each}
			<form class="flex flex-wrap items-center gap-1" onsubmit={addFilter}>
				<label class="sr-only" for="filter-path">Field path</label>
				<input id="filter-path" name="path" placeholder="field.path" maxlength="324" class="{input} w-36 py-1 text-xs" />
				<label class="sr-only" for="filter-op">Operator</label>
				<select id="filter-op" name="op" bind:value={filterOp} class="{input} py-1 text-xs">
					{#each FILTER_OPS as op (op.value)}
						<option value={op.value}>{op.label}</option>
					{/each}
				</select>
				{#if filterNeedsValue(filterOp)}
					<label class="sr-only" for="filter-value">Value</label>
					<input id="filter-value" name="value" placeholder="value" maxlength="1024" class="{input} w-36 py-1 text-xs" />
				{/if}
				<button type="submit" class="{button} btn-sm">Add filter</button>
			</form>
			{#if filterError}<span class="text-xs text-danger-ink" role="alert">{filterError}</span>{/if}
		</div>
	</div>

	{#if data.searchError}
		<div class="alert alert-danger" role="alert">
			{data.searchError}
		</div>
	{/if}

	{#if data.histogram}
		<div class="card p-4">
			<div class="mb-2 flex flex-wrap items-baseline justify-between gap-2">
				<h2 class="card-title">
					{data.histogram.total.toLocaleString('en-US')} logs
				</h2>
				<p class="text-xs text-muted">{formatUtc(data.window.from)} – {formatUtc(data.window.to)}</p>
			</div>
			<LogVolumeChart histogram={data.histogram} onselect={(from, to) => go({ range: 'custom', from, to })} />
		</div>
	{:else if data.histogramError && !data.searchError}
		<div class="alert alert-warning" role="alert">{data.histogramError}</div>
	{/if}

	<div class="card">
		<div class="flex flex-wrap items-center justify-between gap-2 border-b border-border px-4 py-3">
			<div class="flex items-center gap-2 text-sm text-ink-2" role="status" aria-live="polite">
				{#if liveStatus === 'live'}
					<span class="h-2 w-2 animate-pulse rounded-full bg-success"></span><span class="text-success-ink">Live</span>
				{:else if liveStatus === 'connecting'}
					<span class="text-muted">Connecting live tail…</span>
				{:else if liveStatus === 'reconnecting'}
					<span class="text-muted">Reconnecting…</span>
				{/if}
				{#if liveNotice}
					<span class="text-warning-ink">{liveNotice}</span>
				{/if}
			</div>
			<form class="flex flex-wrap items-center gap-1" onsubmit={addColumn}>
				<label class="sr-only" for="column-path">Add a column</label>
				<input id="column-path" name="column" placeholder="Add column: field.path" maxlength="324" class="{input} w-52 py-1 text-xs" />
				<button type="submit" class="{button} btn-sm">Add column</button>
				{#if columnError}<span class="text-xs text-danger-ink" role="alert">{columnError}</span>{/if}
			</form>
		</div>
		{#if shown.length === 0}
			<p class="px-4 py-12 text-center text-sm text-muted">
				{#if data.sources.length === 0 && !data.sourcesError}
					No sources yet — <a href="{path}/sources" class="text-primary">create one</a> to start sending logs.
				{:else}
					No logs match in this time range.
				{/if}
			</p>
		{:else}
			<div class="overflow-x-auto">
				<table class="w-full table-fixed text-left text-sm">
					<thead class="text-[11px] tracking-wider text-muted uppercase">
						<tr class="border-b border-border">
							<th class="w-48 px-4 py-2 font-medium">Time (UTC)</th>
							<th class="w-24 px-2 py-2 font-medium">Level</th>
							<th class="w-36 px-2 py-2 font-medium">Source</th>
							<th class="min-w-64 px-2 py-2 font-medium">Message</th>
							{#each data.state.columns as column (column)}
								<th class="w-40 px-2 py-2 font-medium">
									<span class="flex items-center gap-1">
										<code class="truncate">{column}</code>
										<button
											type="button"
											aria-label="Remove column {column}"
											onclick={() => go({ columns: data.state.columns.filter((other) => other !== column) })}
											class="text-muted hover:text-danger-ink">×</button
										>
									</span>
								</th>
							{/each}
						</tr>
					</thead>
					<tbody>
						{#each shown as row (row.id)}
							{@const open = expanded.has(row.id)}
							<tr class="border-b border-border align-top hover:bg-surface-2">
								<td class="px-4 py-1.5 font-mono text-xs whitespace-nowrap text-ink-2">{formatLogTime(row.dt)}</td>
								<td class="px-2 py-1.5">
									<span class="rounded-full px-2 py-0.5 text-xs font-medium {levelClasses(row.level)}">{row.level}</span>
								</td>
								<td class="truncate px-2 py-1.5 text-xs text-ink-2">{sourceNames.get(row.source_id) ?? (data.sourcesError ? 'unknown source' : 'deleted source')}</td>
								<td class="px-2 py-1.5">
									<button
										type="button"
										aria-expanded={open}
										onclick={() => toggleRow(row)}
										class="block w-full truncate text-left font-mono text-xs text-ink hover:text-primary"
										>{row.message || '(no message)'}</button
									>
								</td>
								{#each data.state.columns as column (column)}
									<td class="truncate px-2 py-1.5 font-mono text-xs text-ink-2">{formatFieldValue(fieldAt(row.fields, column))}</td>
								{/each}
							</tr>
							{#if open}
								{@const full = fullRows.get(row.id) ?? row}
								<tr class="border-b border-border bg-surface-2">
									<td colspan={4 + data.state.columns.length} class="space-y-3 px-4 py-3">
										{#if full.truncated}
											<p class="text-xs text-warning-ink" role="status">
												{#if fullLoading.has(row.id)}
													Shortened in the list — loading the whole entry…
												{:else if fullErrors.has(row.id)}
													Shortened in the list. {fullErrors.get(row.id)}
													<button type="button" onclick={() => loadFull(row)} class="underline hover:text-warning-ink"
														>Retry</button
													>
												{:else}
													Shortened in the list.
												{/if}
											</p>
										{/if}
										<pre class="max-h-96 overflow-auto font-mono text-xs wrap-anywhere whitespace-pre-wrap text-ink">{full.message}</pre>
										<pre
											class="max-h-96 overflow-auto rounded-control border border-border bg-surface-2 p-3 font-mono text-xs wrap-anywhere whitespace-pre-wrap text-ink-2">{JSON.stringify(full.fields, null, 2)}</pre>
										<dl class="grid gap-x-6 gap-y-1 text-xs text-muted sm:grid-cols-2">
											<div>id <code class="text-ink-2">{row.id}</code></div>
											<div>pattern <code class="text-ink-2">{row.pattern_hash}</code></div>
											{#if row.trace_id}<div>trace <code class="text-ink-2">{row.trace_id}</code></div>{/if}
											{#if row.span_id}<div>span <code class="text-ink-2">{row.span_id}</code></div>{/if}
										</dl>
										<CopyButton value={JSON.stringify(full, null, 2)} label="Copy JSON" />
									</td>
								</tr>
							{/if}
						{/each}
					</tbody>
				</table>
			</div>
			<div class="flex items-center justify-center gap-3 px-4 py-3">
				{#if nextCursor}
					<button type="button" onclick={loadMore} disabled={loadingMore} class={button}>
						{loadingMore ? 'Loading…' : 'Load more'}
					</button>
				{:else}
					<span class="text-xs text-muted">End of results in this time range.</span>
				{/if}
				{#if loadError}<span class="text-xs text-danger-ink" role="alert">{loadError}</span>{/if}
			</div>
		{/if}
	</div>
</div>
