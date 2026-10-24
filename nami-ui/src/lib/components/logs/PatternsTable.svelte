<script lang="ts">
	// The window's top message patterns (at most 100, most frequent first).
	// Pattern text comes from log messages, which anyone with a source token
	// can send: it renders as text only. A pattern links to the results
	// limited to it (`href`), so it works without JavaScript too.
	import { formatLogTime, type PatternCount } from '#lib/logs/logs';

	let { patterns, href }: { patterns: PatternCount[]; href: (hash: string) => string } = $props();

	const top = $derived(Math.max(1, ...patterns.map((pattern) => pattern.count)));
</script>

{#if patterns.length === 0}
	<p class="px-4 py-12 text-center text-sm text-muted">No patterns in this time range.</p>
{:else}
	<div class="overflow-x-auto">
		<table class="w-full table-fixed text-left text-sm">
			<thead class="text-[11px] tracking-wider text-muted uppercase">
				<tr class="border-b border-border">
					<th class="min-w-64 px-4 py-2 font-medium">Pattern</th>
					<th class="w-40 px-2 py-2 font-medium">Count</th>
					<th class="w-48 px-2 py-2 font-medium">First seen (UTC)</th>
					<th class="w-48 px-2 py-2 font-medium">Last seen (UTC)</th>
				</tr>
			</thead>
			<tbody>
				{#each patterns as pattern (pattern.pattern_hash)}
					<tr class="border-b border-border align-top hover:bg-surface-2">
						<td class="px-4 py-1.5">
							<a
								href={href(pattern.pattern_hash)}
								class="block font-mono text-xs wrap-anywhere whitespace-pre-wrap text-ink hover:text-primary"
								>{pattern.pattern || '(empty message)'}</a
							>
						</td>
						<td class="px-2 py-1.5">
							<span class="text-xs text-ink-2 tabular-nums">{pattern.count.toLocaleString('en-US')}</span>
							<span class="mt-1 block h-1.5 rounded-full bg-primary-soft" aria-hidden="true">
								<span class="block h-full rounded-full bg-primary" style="width: {(pattern.count / top) * 100}%"></span>
							</span>
						</td>
						<td class="px-2 py-1.5 font-mono text-xs whitespace-nowrap text-ink-2">{formatLogTime(pattern.first_seen)}</td>
						<td class="px-2 py-1.5 font-mono text-xs whitespace-nowrap text-ink-2">{formatLogTime(pattern.last_seen)}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
{/if}
<p class="px-4 py-3 text-xs text-muted">Top {patterns.length} patterns. Counts are approximate for very large windows.</p>
