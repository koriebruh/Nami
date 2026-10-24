<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import Card from '#lib/components/ui/Card.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const isAdmin = $derived(data.role === 'admin');
	const pending = $derived(
		form?.action === 'delete' && 'confirmDelete' in form ? data.policies.find((p) => p.id === form.confirmDelete) : undefined
	);
	const message = $derived(form && 'message' in form && form.action !== 'create' ? form.message : null);
	const failed = $derived(!!form && !('done' in form));

	let busy = $state(false);
	const submit: SubmitFunction = () => {
		busy = true;
		return async ({ update }) => {
			try {
				await update();
			} finally {
				busy = false;
			}
		};
	};
	// A delete asks with confirm() first, then sends `confirm=yes`. Without
	// JavaScript the action asks on the page (the panel below).
	const askDelete: SubmitFunction = (submission) => {
		if (!confirm(`Delete policy "${submission.formData.get('name')}"?`)) {
			submission.cancel();
			return;
		}
		submission.formData.set('confirm', 'yes');
		return submit(submission);
	};
	const steps = (n: number) => `${n} ${n === 1 ? 'step' : 'steps'}`;
</script>

<svelte:head>
	<title>Escalation · {data.project.name}</title>
</svelte:head>

<div class="space-y-6">
	<Card title="Escalation policies" subtitle="Who is notified about an incident, in which order, and after how long." padded={false}>
		{#if message}
			<div class="alert {failed ? 'alert-danger' : 'alert-success'} m-4" role={failed ? 'alert' : 'status'}>{message}</div>
		{/if}
		{#if isAdmin && pending}
			<form method="POST" action="?/delete" use:enhance={submit} class="alert alert-danger m-4 flex flex-wrap items-center gap-3">
				<input type="hidden" name="policy_id" value={pending.id} />
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">Delete policy "{pending.name}"?</span>
				<button type="submit" disabled={busy} class="btn btn-danger-solid btn-sm">Yes, delete</button>
				<a href="?" class="text-ink-2 hover:text-ink">Cancel</a>
			</form>
		{/if}
		{#if data.policies.length === 0}
			<EmptyState icon="bell" message="No escalation policies." />
		{:else}
			<div class="overflow-x-auto">
				<table class="table table-hover">
					<thead>
						<tr>
							<th>Name</th>
							<th>Steps</th>
							<th>Repeats</th>
							{#if isAdmin}<th><span class="sr-only">Actions</span></th>{/if}
						</tr>
					</thead>
					<tbody>
						{#each data.policies as policy (policy.id)}
							<tr>
								<td>
									<a href="/projects/{data.project.id}/escalation/{policy.id}" class="link wrap-anywhere">{policy.name}</a>
									{#if policy.is_default}<span class="badge badge-primary ml-2">Default</span>{/if}
								</td>
								<td>{steps(policy.steps.length)}</td>
								<td>{policy.repeat_count}</td>
								{#if isAdmin}
									<td>
										<div class="flex flex-wrap justify-end gap-2">
											{#if !policy.is_default}
												<form method="POST" action="?/makeDefault" use:enhance={submit}>
													<input type="hidden" name="policy_id" value={policy.id} />
													<button type="submit" disabled={busy} class="btn btn-secondary btn-sm">Make default</button>
												</form>
											{/if}
											<form method="POST" action="?/delete" use:enhance={askDelete}>
												<input type="hidden" name="policy_id" value={policy.id} />
												<input type="hidden" name="name" value={policy.name} />
												<button type="submit" disabled={busy} class="btn btn-danger btn-sm">Delete</button>
											</form>
										</div>
									</td>
								{/if}
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</Card>

	{#if isAdmin}
		<Card title="New policy" subtitle="It starts with one step that notifies all members at once; change it in the editor.">
			{#if form?.action === 'create' && 'message' in form}
				<div role="alert" class="alert alert-danger mb-4">{form.message}</div>
			{/if}
			<form method="POST" action="?/create" use:enhance={submit} class="flex flex-wrap items-end gap-3">
				<input type="hidden" name="step_count" value="1" />
				<input type="hidden" name="step_0_delay" value="0" />
				<input type="hidden" name="step_0_target" value="all_members" />
				<div class="min-w-[14rem] flex-1">
					<label for="policy-name" class="label">Name</label>
					<input id="policy-name" name="name" type="text" required maxlength="200" placeholder="Business hours" class="input" />
				</div>
				<button type="submit" disabled={busy} class="btn btn-primary">Create policy</button>
			</form>
		</Card>
	{:else}
		<p class="text-sm text-muted">Only project admins can change escalation policies.</p>
	{/if}
</div>
