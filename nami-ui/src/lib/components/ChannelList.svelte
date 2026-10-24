<!-- Notification channels of one owner (the caller, or a project), shared by
     the personal and the project page. Secrets are write-only: no input is
     ever rendered with a stored value, only the engine's kind and summary. -->
<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import EmptyState from '#lib/components/ui/EmptyState.svelte';
	import {
		CHANNEL_FIELDS,
		CHANNEL_LABELS,
		isChannelKind,
		kindsFor,
		type Channel,
		type ChannelKind
	} from '#lib/incidents/channels';

	type ChannelForm = {
		action?: string;
		message?: string;
		created?: boolean;
		signingSecret?: string;
		channel?: string;
		saved?: boolean;
		tested?: string;
		ok?: boolean;
		confirmDelete?: string;
		deleted?: boolean;
	} | null;

	let {
		channels,
		form,
		scope,
		canEdit
	}: { channels: Channel[]; form: ChannelForm | undefined; scope: 'personal' | 'project'; canEdit: boolean } = $props();

	const kinds = $derived(kindsFor(scope));
	// The initial kind only: scope never changes on a page.
	// svelte-ignore state_referenced_locally
	let kind = $state<ChannelKind>(kindsFor(scope)[0]);
	const label = (k: string) => (isChannelKind(k) ? CHANNEL_LABELS[k] : k);
	const fieldsOf = (k: string) => (isChannelKind(k) ? CHANNEL_FIELDS[k] : []);

	// Buttons stay disabled from a submit until its result is applied.
	let busy = $state(false);
	const submit: SubmitFunction = () => {
		busy = true;
		return async ({ update }) => {
			try {
				await update({ reset: false });
			} finally {
				busy = false;
			}
		};
	};
	// A created channel empties the form (its secrets with it).
	const submitCreate: SubmitFunction = () => {
		busy = true;
		return async ({ update, result }) => {
			try {
				await update({ reset: result.type === 'success' });
			} finally {
				busy = false;
			}
		};
	};
	// Delete asks with confirm() first, then sends `confirm=yes`. Without
	// JavaScript the action asks on the page (the second step below).
	const submitDelete: SubmitFunction = (submission) => {
		const name = String(submission.formData.get('channel_name') ?? '');
		if (!confirm(`Delete channel "${name}"?`)) {
			submission.cancel();
			return;
		}
		submission.formData.set('confirm', 'yes');
		return submit(submission);
	};

	const pending = $derived(form?.confirmDelete ? channels.find((c) => c.id === form.confirmDelete) : undefined);
</script>

<div class="space-y-6">
	{#if form?.signingSecret}
		<div class="space-y-3 rounded-card border border-success-ink/25 bg-success-soft p-5 sm:p-6" role="status">
			<p class="text-sm text-success-ink">Webhook created. This is its signing secret. Shown once. Store it now.</p>
			<div role="group" aria-label="Signing secret" class="flex items-center gap-2">
				<code
					class="block flex-1 rounded-control border border-border bg-surface-2 px-3 py-2 font-mono text-xs break-all text-ink select-all"
					>{form.signingSecret}</code
				>
				<CopyButton value={form.signingSecret} />
			</div>
			<p class="text-xs text-ink-2">
				Each request has <code>X-Nami-Timestamp</code> and <code>X-Nami-Signature: sha256=&lt;hex&gt;</code>, the
				HMAC-SHA256 of <code>{'{X-Nami-Timestamp}.{raw body}'}</code> with this secret. Reject requests whose timestamp
				is more than 5 minutes old, and drop repeats of <code>X-Nami-Delivery</code>.
			</p>
		</div>
	{:else if form?.created}
		<div class="alert alert-success" role="status">Channel created.</div>
	{/if}

	{#if form?.action === 'delete' && form.message}
		<div class="alert {form.deleted ? 'alert-success' : 'alert-danger'}" role={form.deleted ? 'status' : 'alert'}>
			{form.message}
		</div>
	{/if}
	{#if canEdit && pending}
		<form method="POST" action="?/delete" use:enhance={submit} class="alert alert-danger flex flex-wrap items-center gap-3">
			<input type="hidden" name="channel_id" value={pending.id} />
			<input type="hidden" name="confirm" value="yes" />
			<span class="wrap-anywhere">Delete channel "{pending.name}"?</span>
			<button type="submit" disabled={busy} class="btn btn-danger-solid btn-sm">Yes, delete</button>
			<a href="?" class="text-ink-2 hover:text-ink">Cancel</a>
		</form>
	{/if}

	<div class="card">
		{#if channels.length === 0}
			<EmptyState icon="bell" message="No channels yet." />
		{:else}
			<ul class="divide-y divide-border">
				{#each channels as channel (channel.id)}
					<li class="space-y-3 px-5 py-4 sm:px-6">
						<div class="flex flex-wrap items-center gap-3">
							<div class="min-w-0 flex-1">
								<p class="font-medium wrap-anywhere text-ink">{channel.name}</p>
								<p class="text-xs wrap-anywhere text-muted">{label(channel.kind)} · {channel.summary}</p>
							</div>
							<span class="badge {channel.enabled ? 'badge-success' : 'badge-neutral'}">
								{channel.enabled ? 'Enabled' : 'Disabled'}
							</span>
							{#if canEdit}
								<form method="POST" action="?/test" use:enhance={submit}>
									<input type="hidden" name="channel_id" value={channel.id} />
									<button type="submit" disabled={busy} class="btn btn-secondary btn-sm">Send test</button>
								</form>
								<form method="POST" action="?/delete" use:enhance={submitDelete}>
									<input type="hidden" name="channel_id" value={channel.id} />
									<input type="hidden" name="channel_name" value={channel.name} />
									<button type="submit" disabled={busy} class="btn btn-danger btn-sm">Delete</button>
								</form>
							{/if}
						</div>

						{#if form?.tested === channel.id}
							<div class="alert {form.ok ? 'alert-success' : 'alert-danger'}" role={form.ok ? 'status' : 'alert'}>
								{form.ok ? 'Test sent.' : form.message}
							</div>
						{/if}
						{#if form?.action === 'update' && form.channel === channel.id && form.message}
							<div class="alert {form.saved ? 'alert-success' : 'alert-danger'}" role={form.saved ? 'status' : 'alert'}>
								{form.message}
							</div>
						{/if}

						{#if canEdit}
							<details>
								<summary class="cursor-pointer text-sm text-ink-2 hover:text-ink">Edit</summary>
								<form method="POST" action="?/update" use:enhance={submit} class="mt-3 grid gap-3 sm:grid-cols-2">
									<input type="hidden" name="channel_id" value={channel.id} />
									<input type="hidden" name="kind" value={channel.kind} />
									<div>
										<label for="name-{channel.id}" class="label">Name</label>
										<input
											id="name-{channel.id}"
											name="name"
											type="text"
											required
											maxlength="100"
											defaultValue={channel.name}
											class="input"
										/>
									</div>
									<label class="flex items-center gap-2 self-end text-sm text-ink">
										<input type="checkbox" name="enabled" defaultChecked={channel.enabled} />
										Enabled
									</label>
									{#each fieldsOf(channel.kind) as field (field.name)}
										<div>
											<label for="{field.name}-{channel.id}" class="label">{field.label}</label>
											<input
												id="{field.name}-{channel.id}"
												name={field.name}
												type={field.secret ? 'password' : 'text'}
												autocomplete="off"
												placeholder="Leave blank to keep the current value"
												class="input"
											/>
										</div>
									{/each}
									<div class="sm:col-span-2">
										<button type="submit" disabled={busy} class="btn btn-primary btn-sm">Save</button>
									</div>
								</form>
							</details>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</div>

	{#if canEdit}
		<div class="card p-5 sm:p-6">
			<h2 class="text-lg font-light text-ink">New channel</h2>
			{#if form?.action === 'create' && form.message}
				<div class="alert alert-danger mt-4" role="alert">{form.message}</div>
			{/if}
			<form method="POST" action="?/create" use:enhance={submitCreate} class="mt-4 grid gap-4 sm:grid-cols-2">
				<div>
					<label for="channel-name" class="label">Name</label>
					<input id="channel-name" name="name" type="text" required maxlength="100" placeholder="On-call Slack" class="input" />
				</div>
				<div>
					<label for="channel-kind" class="label">Kind</label>
					<select id="channel-kind" name="kind" bind:value={kind} class="input">
						{#each kinds as k (k)}
							<option value={k}>{CHANNEL_LABELS[k]}</option>
						{/each}
					</select>
				</div>
				{#each CHANNEL_FIELDS[kind] as field (field.name)}
					<div>
						<label for="new-{field.name}" class="label">{field.label}</label>
						<input
							id="new-{field.name}"
							name={field.name}
							type={field.secret ? 'password' : kind === 'email' ? 'email' : 'text'}
							required
							autocomplete="off"
							placeholder={field.placeholder}
							class="input"
						/>
					</div>
				{/each}
				<div class="sm:col-span-2">
					<button type="submit" disabled={busy} class="btn btn-primary">Create</button>
				</div>
			</form>
		</div>
	{/if}
</div>
