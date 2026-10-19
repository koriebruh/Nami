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
		LIVE_ROW_CAP,
		MAX_COLUMNS,
		MAX_FILTERS,
		PAGE_SIZE,
		tailParams,
		TIME_PRESETS,
		toggled,
		toInputValue,
		type ExplorerState,
		type FieldFilter,
		type FilterOp,
		type LogEntry
	} from '#lib/logs';
	import { formatUtc } from '#lib/uptime';
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

	async function loadMore() {
		if (!nextCursor || loadingMore) return;
		loadingMore = true;
		loadError = null;
		const params = engineParams(data.state, data.window);
		params.set('cursor', nextCursor);
		params.set('limit', String(PAGE_SIZE));
		try {
			const response = await fetch(`${path}/search?${params}`);
			if (response.status === 401) {
				await goto('/login');
				return;
			}
			const body = await response.json().catch(() => null);
			if (response.ok && body) {
				rows = [...rows, ...body.logs];
				nextCursor = body.next_cursor;
			} else {
				loadError = body?.error ?? 'Could not load more logs.';
			}
		} catch {
			loadError = 'Could not load more logs.';
		} finally {
			loadingMore = false;
		}
	}

	// Live tail: Server-Sent Events through the BFF while `live` is on. The
	// stream closes when the toggle goes off, on navigation (effect cleanup)
	// and when the page is hidden for good (pagehide); rows held are capped.
	let liveRows = $state<LogEntry[]>([]);
	let liveStatus = $state<'off' | 'connecting' | 'live' | 'reconnecting' | 'ended'>('off');
	let liveNotice = $state<string | null>(null);
	$effect(() => {
		if (!data.state.live) {
			liveStatus = 'off';
			liveNotice = null;
			return;
		}
		const query = tailParams(data.state).toString();
		const stream = new EventSource(`${path}/tail${query ? `?${query}` : ''}`);
		liveRows = [];
		liveNotice = null;
		liveStatus = 'connecting';
		stream.onopen = () => {
			liveStatus = 'live';
			liveNotice = null;
		};
		stream.addEventListener('log', (event) => {
			let row: LogEntry;
			try {
				row = JSON.parse((event as MessageEvent).data);
			} catch {
				return;
			}
			liveRows = [row, ...liveRows].slice(0, LIVE_ROW_CAP);
		});
		// The engine ends a stream after 30 minutes ("timeout": EventSource
		// reconnects by itself) or when the session is no longer valid
		// ("session": no reconnect, it would only be refused again).
		stream.addEventListener('end', (event) => {
			let reason: unknown;
			try {
				reason = JSON.parse((event as MessageEvent).data).reason;
			} catch {
				reason = null;
			}
			if (reason === 'session') {
				stream.close();
				liveStatus = 'ended';
				liveNotice = 'Live tail stopped: your session ended or access changed. Reload the page to resume.';
			} else {
				liveStatus = 'reconnecting';
				liveNotice = 'The live stream reached its time limit and is reconnecting…';
			}
		});
		stream.onerror = () => {
			if (stream.readyState === EventSource.CLOSED) {
				liveStatus = 'ended';
				liveNotice = 'Live tail could not be started (check the filters, or sign in again). Reload to retry.';
			} else if (liveStatus === 'live') {
				liveStatus = 'reconnecting';
			}
		};
		const hide = () => {
			stream.close();
			liveStatus = 'ended';
			liveNotice = 'Live tail paused while the page was hidden. Reload to resume.';
		};
		window.addEventListener('pagehide', hide);
		return () => {
			window.removeEventListener('pagehide', hide);
			stream.close();
		};
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

	const input =
		'rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-1.5 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const button =
		'rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100';
	const chip = (on: boolean) =>
		`rounded-md px-2.5 py-1 text-xs transition-colors ${on ? 'bg-[#1f1f1f] text-neutral-100' : 'text-neutral-500 hover:text-neutral-300'}`;
</script>

<svelte:head>
	<title>Explorer · Logs · {data.project.name}</title>
</svelte:head>

<div class="space-y-4">
	<div class="space-y-3 rounded-xl border border-[#262626] bg-[#141414] p-4">
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
				<label class="flex items-center gap-2 text-sm text-neutral-400">
					<input type="checkbox" name="in_fields" defaultChecked={data.state.inFields} /> Also search fields
				</label>
				<button type="submit" class="rounded-md bg-teal-500 px-4 py-1.5 text-sm font-medium text-neutral-950 hover:bg-teal-400"
					>Search</button
				>
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
					<div class="absolute right-0 z-10 mt-1 w-64 space-y-1 rounded-md border border-[#262626] bg-[#141414] p-2 shadow-lg">
						{#each data.sources as source (source.id)}
							<label class="flex items-center gap-2 rounded px-2 py-1 text-sm text-neutral-300 hover:bg-[#1f1f1f]">
								<input
									type="checkbox"
									checked={data.state.sources.includes(source.id)}
									onchange={() => go({ sources: toggled(data.state.sources, source.id) })}
								/>
								{source.name}
							</label>
						{:else}
							<p class="px-2 py-1 text-sm text-neutral-500">
								No sources yet — <a href="{path}/sources" class="text-teal-300">create one</a>.
							</p>
						{/each}
					</div>
				</details>
				<button
					type="button"
					aria-pressed={data.state.live}
					onclick={() => go({ live: !data.state.live })}
					class="{button} {data.state.live ? 'border-teal-500/40 text-teal-300' : ''}"
				>
					{data.state.live ? 'Stop live tail' : 'Live tail'}
				</button>
			</div>
		</div>

		{#if showCustom || data.state.range === 'custom'}
			<form class="flex flex-wrap items-center gap-2 text-sm text-neutral-400" onsubmit={applyCustom}>
				<label for="log-from">From</label>
				<input id="log-from" name="from" type="datetime-local" defaultValue={toInputValue(data.window.from)} class={input} />
				<label for="log-to">to</label>
				<input id="log-to" name="to" type="datetime-local" defaultValue={toInputValue(data.window.to)} class={input} />
				<span class="text-xs text-neutral-600">UTC</span>
				<button type="submit" class={button}>Apply</button>
				{#if rangeError}<span class="text-red-400" role="alert">{rangeError}</span>{/if}
			</form>
		{/if}

		<div class="flex flex-wrap items-center gap-1" role="group" aria-label="Levels">
			{#each LEVELS as level (level)}
				{@const on = data.state.levels.includes(level)}
				<button
					type="button"
					aria-pressed={on}
					onclick={() => go({ levels: toggled(data.state.levels, level) })}
					class="rounded-md px-2 py-0.5 text-xs transition-colors {on
						? `${levelClasses(level)} ring-1 ring-current`
						: 'text-neutral-500 hover:text-neutral-300'}">{level}</button
				>
			{/each}
		</div>

		<div class="flex flex-wrap items-center gap-2">
			{#each data.state.filters as filter, index (index)}
				<span class="flex items-center gap-1 rounded-md border border-[#262626] bg-[#0a0a0a] px-2 py-0.5 text-xs text-neutral-300">
					<code>{filter.path}</code>
					<span class="text-neutral-500">{opLabels.get(filter.op)}</span>
					{#if filter.value !== undefined}<code class="text-teal-300">{filter.value}</code>{/if}
					<button
						type="button"
						aria-label="Remove filter on {filter.path}"
						onclick={() => go({ filters: data.state.filters.filter((_, other) => other !== index) })}
						class="text-neutral-500 hover:text-red-400">×</button
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
				<button type="submit" class="{button} py-1 text-xs">Add filter</button>
			</form>
			{#if filterError}<span class="text-xs text-red-400" role="alert">{filterError}</span>{/if}
		</div>
	</div>

	{#if data.searchError}
		<div class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400" role="alert">
			{data.searchError}
		</div>
	{/if}

	{#if data.histogram}
		<div class="rounded-xl border border-[#262626] bg-[#141414] p-4">
			<div class="mb-2 flex flex-wrap items-baseline justify-between gap-2">
				<h2 class="text-sm font-semibold text-neutral-200">
					{data.histogram.total.toLocaleString('en-US')} logs
				</h2>
				<p class="text-xs text-neutral-500">{formatUtc(data.window.from)} – {formatUtc(data.window.to)}</p>
			</div>
			<LogVolumeChart histogram={data.histogram} onselect={(from, to) => go({ range: 'custom', from, to })} />
		</div>
	{/if}

	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<div class="flex flex-wrap items-center justify-between gap-2 border-b border-[#262626] px-4 py-3">
			<div class="flex items-center gap-2 text-sm text-neutral-400">
				{#if liveStatus === 'live'}
					<span class="h-2 w-2 animate-pulse rounded-full bg-teal-400"></span><span class="text-teal-300">Live</span>
				{:else if liveStatus === 'connecting'}
					<span class="text-neutral-500">Connecting live tail…</span>
				{:else if liveStatus === 'reconnecting'}
					<span class="text-neutral-500">Reconnecting…</span>
				{/if}
				{#if liveNotice}
					<span class="text-amber-300" role="status">{liveNotice}</span>
				{/if}
			</div>
			<form class="flex flex-wrap items-center gap-1" onsubmit={addColumn}>
				<label class="sr-only" for="column-path">Add a column</label>
				<input id="column-path" name="column" placeholder="Add column: field.path" maxlength="324" class="{input} w-52 py-1 text-xs" />
				<button type="submit" class="{button} py-1 text-xs">Add column</button>
				{#if columnError}<span class="text-xs text-red-400" role="alert">{columnError}</span>{/if}
			</form>
		</div>
		{#if shown.length === 0}
			<p class="px-4 py-10 text-center text-sm text-neutral-500">
				{#if data.sources.length === 0}
					No sources yet — <a href="{path}/sources" class="text-teal-300">create one</a> to start sending logs.
				{:else}
					No logs match in this time range.
				{/if}
			</p>
		{:else}
			<div class="overflow-x-auto">
				<table class="w-full table-fixed text-left text-sm">
					<thead class="text-xs text-neutral-500">
						<tr class="border-b border-[#262626]">
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
											class="text-neutral-500 hover:text-red-400">×</button
										>
									</span>
								</th>
							{/each}
						</tr>
					</thead>
					<tbody>
						{#each shown as row (row.id)}
							{@const open = expanded.has(row.id)}
							<tr class="border-b border-[#262626] align-top hover:bg-[#181818]">
								<td class="px-4 py-1.5 font-mono text-xs whitespace-nowrap text-neutral-400">{formatLogTime(row.dt)}</td>
								<td class="px-2 py-1.5">
									<span class="rounded px-1.5 py-0.5 text-xs {levelClasses(row.level)}">{row.level}</span>
								</td>
								<td class="truncate px-2 py-1.5 text-xs text-neutral-400">{sourceNames.get(row.source_id) ?? 'deleted source'}</td>
								<td class="px-2 py-1.5">
									<button
										type="button"
										aria-expanded={open}
										onclick={() => toggleRow(row)}
										class="block w-full truncate text-left font-mono text-xs text-neutral-200 hover:text-white"
										>{row.message || '(no message)'}</button
									>
								</td>
								{#each data.state.columns as column (column)}
									<td class="truncate px-2 py-1.5 font-mono text-xs text-neutral-400">{formatFieldValue(fieldAt(row.fields, column))}</td>
								{/each}
							</tr>
							{#if open}
								{@const full = fullRows.get(row.id) ?? row}
								<tr class="border-b border-[#262626] bg-[#0f0f0f]">
									<td colspan={4 + data.state.columns.length} class="space-y-3 px-4 py-3">
										{#if full.truncated}
											<p class="text-xs text-amber-300" role="status">
												{#if fullLoading.has(row.id)}
													Shortened in the list — loading the whole entry…
												{:else if fullErrors.has(row.id)}
													Shortened in the list. {fullErrors.get(row.id)}
													<button type="button" onclick={() => loadFull(row)} class="underline hover:text-amber-200"
														>Retry</button
													>
												{:else}
													Shortened in the list.
												{/if}
											</p>
										{/if}
										<pre class="max-h-96 overflow-auto font-mono text-xs wrap-anywhere whitespace-pre-wrap text-neutral-200">{full.message}</pre>
										<pre
											class="max-h-96 overflow-auto rounded-md border border-[#262626] bg-[#0a0a0a] p-3 font-mono text-xs text-neutral-300">{JSON.stringify(full.fields, null, 2)}</pre>
										<dl class="grid gap-x-6 gap-y-1 text-xs text-neutral-500 sm:grid-cols-2">
											<div>id <code class="text-neutral-400">{row.id}</code></div>
											<div>pattern <code class="text-neutral-400">{row.pattern_hash}</code></div>
											{#if row.trace_id}<div>trace <code class="text-neutral-400">{row.trace_id}</code></div>{/if}
											{#if row.span_id}<div>span <code class="text-neutral-400">{row.span_id}</code></div>{/if}
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
					<span class="text-xs text-neutral-600">End of results in this time range.</span>
				{/if}
				{#if loadError}<span class="text-xs text-red-400" role="alert">{loadError}</span>{/if}
			</div>
		{/if}
	</div>
</div>
