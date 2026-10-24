<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import LogAlertForm from '#lib/components/logs/LogAlertForm.svelte';
	import Badge from '#lib/components/ui/Badge.svelte';
	import { describeCondition } from '#lib/logs/log-alerts';
	import { formatUtc } from '#lib/uptime/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const list = $derived(`/projects/${data.project.id}/logs/alerts`);
	const base = $derived(`${list}/${data.alert.id}`);

	let busy = $state(false);
	const submitWith =
		(reset: boolean): SubmitFunction =>
		() => {
			busy = true;
			return async ({ update }) => {
				try {
					await update({ reset });
				} finally {
					busy = false;
				}
			};
		};
	// Delete asks with confirm() first; the dialog is the confirmation, so the
	// form sends `confirm=yes`. Without JavaScript the action asks on the page.
	const deleteQuestion = $derived(`Delete log alert "${data.alert.name}"? Its incidents are deleted with it.`);
	const confirmed: SubmitFunction = (submission) => {
		if (!confirm(deleteQuestion)) {
			submission.cancel();
			return;
		}
		submission.formData.set('confirm', 'yes');
		return submitWith(true)(submission);
	};
</script>

<svelte:head>
	<title>{data.alert.name} · Alerts · Logs</title>
</svelte:head>

<div class="space-y-6">
	<a href={list} class="text-sm text-muted hover:text-primary">← Alerts</a>

	<div class="card p-5 sm:p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div class="space-y-1">
				<h2 class="text-lg font-light wrap-anywhere text-ink">{data.alert.name}</h2>
				<p class="flex flex-wrap items-center gap-2 text-sm text-muted">
					<Badge tone={data.alert.state === 'alerting' ? 'danger' : 'neutral'} dot>
						{data.alert.state === 'alerting' ? 'Alerting' : 'OK'}
					</Badge>
					{#if !data.alert.enabled}<Badge tone="neutral">Disabled</Badge>{/if}
					<span>Alerts on {describeCondition(data.alert)}</span>
				</p>
				<p class="text-xs text-muted">
					{#if data.alert.last_evaluated_at}
						Last evaluated {formatUtc(data.alert.last_evaluated_at)}: {(data.alert.last_count ?? 0).toLocaleString('en-US')}
						matching
					{:else}
						Not evaluated yet
					{/if}
					· next check {formatUtc(data.alert.next_evaluation_at)}
				</p>
				{#if data.alert.last_error}
					<p class="text-xs wrap-anywhere text-danger-ink">
						Could not evaluate: {data.alert.last_error}{#if data.alert.last_error_at}
							({formatUtc(data.alert.last_error_at)}){/if}
					</p>
				{/if}
			</div>
			<div class="flex flex-wrap items-center gap-2">
				<a href="/projects/{data.project.id}/incidents" class="btn btn-secondary btn-sm">Incidents</a>
				<form method="POST" action="?/delete" use:enhance={confirmed}>
					<button type="submit" class="btn btn-danger btn-sm" disabled={busy}>Delete</button>
				</form>
			</div>
		</div>
		{#if form?.message && form.action === 'delete'}
			<div role="alert" class="alert alert-danger mt-4">{form.message}</div>
		{/if}
		<!-- The second step of a delete without JavaScript (see the delete action). -->
		{#if form?.confirmDelete}
			<form method="POST" action="?/delete" use:enhance={submitWith(true)} class="alert alert-danger mt-4 flex flex-wrap items-center gap-3">
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">{deleteQuestion}</span>
				<button type="submit" class="btn btn-danger-solid btn-sm" disabled={busy}>Yes, delete</button>
				<a href={base} class="text-ink-2 hover:text-ink">Cancel</a>
			</form>
		{/if}
	</div>

	<div class="card space-y-4 p-5 sm:p-6">
		<h3 class="card-title">Settings</h3>
		{#if form?.saved}
			<div role="status" class="alert alert-success">Saved. The alert is checked again now.</div>
		{/if}
		{#if form?.message && form.action === 'update'}
			<div role="alert" class="alert alert-danger">{form.message}</div>
		{/if}
		<!-- Keyed, so that opening another alert starts from its own values. -->
		{#key data.alert.id}
			<form method="POST" action="?/update" use:enhance={submitWith(false)}>
				<LogAlertForm
					initial={data.alert}
					sources={data.sources}
					policies={data.policies}
					severities={data.severities}
					previewUrl="{list}/preview"
					submitLabel="Save"
					{busy}
				/>
			</form>
		{/key}
	</div>
</div>
