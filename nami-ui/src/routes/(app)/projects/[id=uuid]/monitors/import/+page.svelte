<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import Badge from '#lib/components/ui/Badge.svelte';
	import Card from '#lib/components/ui/Card.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import Input from '#lib/components/ui/Input.svelte';
	import PageHeader from '#lib/components/ui/PageHeader.svelte';
	import Select from '#lib/components/ui/Select.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const FORMATS = [
		{ value: 'urls', label: 'URL list', hint: 'One URL per line.' },
		{ value: 'uptime_kuma', label: 'Uptime Kuma', hint: 'Uptime Kuma 1.x backup (Settings → Backup → Export).' },
		{ value: 'uptimerobot', label: 'UptimeRobot', hint: 'UptimeRobot monitors CSV export.' }
	] as const;

	const MAX_BYTES = 2 * 1024 * 1024;

	// Read once: the typed text stays in the DOM (the action echoes it back for
	// the no-JS path), later prop updates must not overwrite it.
	function initialFormat(): string {
		return form?.format ?? 'urls';
	}
	let format = $state(initialFormat());
	let fileError = $state<string | null>(null);

	// Both buttons stay disabled from the submit until the result is applied,
	// so a double click cannot start two imports.
	let submitting = $state(false);
	const submit: SubmitFunction = ({ cancel }) => {
		if (submitting) {
			cancel();
			return;
		}
		submitting = true;
		return async ({ update }) => {
			try {
				// reset: false keeps the pasted text and the chosen file.
				await update({ reset: false });
			} finally {
				submitting = false;
			}
		};
	};

	function onfile(event: Event) {
		const file = (event.currentTarget as HTMLInputElement).files?.[0];
		fileError = file && file.size > MAX_BYTES ? 'The file is larger than 2 MiB' : null;
	}

	const report = $derived(form && 'report' in form ? form.report : null);
	const message = $derived(form && 'message' in form ? (form.message as string) : null);
	const TONE = { created: 'success', skipped: 'neutral', error: 'danger' } as const;
	const statusLabel = (status: keyof typeof TONE, dry: boolean) =>
		status === 'created' ? (dry ? 'Will be created' : 'Created') : status === 'skipped' ? 'Skipped' : 'Error';
</script>

<svelte:head>
	<title>Import monitors · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<PageHeader title="Import monitors" subtitle="Bring monitors over from a URL list, Uptime Kuma or UptimeRobot.">
		{#snippet eyebrow()}
			<a href="/projects/{data.project.id}/monitors" class="hover:text-ink">← Monitors</a>
		{/snippet}
	</PageHeader>

	<Card title="Source">
		<!-- A plain multipart POST works without JavaScript: the two buttons pick
		     the action through formaction. -->
		<form method="POST" enctype="multipart/form-data" use:enhance={submit} class="space-y-5">
			{#if message}
				<div role="alert" class="alert alert-danger">{message}</div>
			{/if}

			<fieldset class="space-y-2">
				<legend class="label">Format</legend>
				<div class="space-y-2">
					{#each FORMATS as option (option.value)}
						<label class="flex items-start gap-2 text-sm text-ink-2">
							<input type="radio" name="format" value={option.value} bind:group={format} class="mt-1" />
							<span><span class="font-medium text-ink">{option.label}</span> — {option.hint}</span>
						</label>
					{/each}
				</div>
			</fieldset>

			<div>
				<label for="content" class="label">Paste the content</label>
				<textarea
					id="content"
					name="content"
					rows="10"
					spellcheck="false"
					class="input font-mono text-xs"
					defaultValue={form?.content ?? ''}
					aria-describedby="content-hint"
				></textarea>
				<p id="content-hint" class="hint">Up to 2 MiB and 500 entries.</p>
			</div>

			<div>
				<label for="file" class="label">Or choose a file</label>
				<input id="file" name="file" type="file" accept=".txt,.json,.csv" onchange={onfile} class="input" aria-describedby="file-hint" />
				<p id="file-hint" class="hint">A chosen file is used instead of the pasted text.</p>
				{#if fileError}<p role="alert" class="mt-1 text-sm text-danger-ink">{fileError}</p>{/if}
			</div>

			<fieldset class="space-y-4 rounded-card border border-border bg-surface-2/60 p-4 sm:p-5">
				<legend class="label px-1">Defaults for what the source leaves out</legend>
				<div class="grid gap-4 sm:grid-cols-3">
					<Input id="interval_seconds" name="interval_seconds" type="number" min="1" inputmode="numeric" label="Interval (s)" placeholder="60" />
					<Input id="timeout_seconds" name="timeout_seconds" type="number" min="1" inputmode="numeric" label="Timeout (s)" placeholder="10" />
					<Input id="confirmation_threshold" name="confirmation_threshold" type="number" min="1" inputmode="numeric" label="Confirmations" placeholder="2" />
				</div>
				<Select id="group_id" name="group_id" label="Group" class="sm:w-1/2">
					<option value="">No group</option>
					{#each data.groups as group (group.id)}
						<option value={group.id}>{group.name}</option>
					{/each}
				</Select>
				<label class="flex items-center gap-2 text-sm text-ink-2">
					<input type="checkbox" name="skip_existing" checked />
					Skip monitors that already exist
				</label>
			</fieldset>

			<div class="flex flex-wrap items-center gap-3">
				<button type="submit" formaction="?/preview" class="btn btn-secondary" disabled={submitting}>Preview</button>
				<button type="submit" formaction="?/import" class="btn btn-primary" disabled={submitting}>Import</button>
				{#if submitting}<span role="status" class="text-sm text-muted">Working…</span>{/if}
			</div>
		</form>
	</Card>

	{#if report}
		<Card
			title={report.dry_run ? 'Preview' : 'Import result'}
			subtitle={report.dry_run ? 'Nothing was created yet.' : undefined}
			padded={false}
		>
			<div role="status" class="flex flex-wrap gap-2 border-b border-border px-5 py-3 text-sm sm:px-6">
				<Badge tone="success">{report.created} {report.dry_run ? 'to create' : 'created'}</Badge>
				<Badge tone="neutral">{report.skipped} skipped</Badge>
				<Badge tone={report.errors > 0 ? 'danger' : 'neutral'}>{report.errors} with errors</Badge>
			</div>
			{#if report.results.length === 0}
				<EmptyState icon="inbox" message="The source holds no entries." />
			{:else}
				<div class="overflow-x-auto">
					<table class="table min-w-[44rem]">
						<thead>
							<tr><th>Line</th><th>Status</th><th>Name</th><th>Target</th><th>Details</th></tr>
						</thead>
						<tbody>
							{#each report.results as row (row.line)}
								<tr>
									<td class="tabular-nums">{row.line}</td>
									<td><Badge tone={TONE[row.status]}>{statusLabel(row.status, report.dry_run)}</Badge></td>
									<td class="wrap-anywhere">
										{#if row.monitor_id && !report.dry_run}
											<a href="/projects/{data.project.id}/monitors/{row.monitor_id}" class="font-medium text-ink hover:text-primary">{row.name}</a>
										{:else}{row.name ?? ''}{/if}
									</td>
									<td class="font-mono text-xs wrap-anywhere">{row.target ?? ''}</td>
									<td class="wrap-anywhere">
										{#if row.error}<span class="text-danger-ink">{row.error}</span>{/if}
										{#if row.warning}<span class="text-warning-ink">{row.warning}</span>{/if}
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</Card>
	{/if}
</div>
