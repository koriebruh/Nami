<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const input = 'input';

	// Buttons stay disabled from a submit until its result is applied, so a
	// double click cannot send an action twice. reset: false keeps the inputs
	// showing what was typed or saved.
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
	// A saved new group empties its form.
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
	// Delete asks with confirm() first; the dialog is the confirmation, so the
	// form sends `confirm=yes`. Without JavaScript the action asks on the page.
	const submitRow: SubmitFunction = (submission) => {
		if (submission.action.search === '?/deleteGroup') {
			const name = String(submission.formData.get('name') ?? '');
			if (!confirm(`Delete group "${name}"? Its monitors stay, ungrouped.`)) {
				submission.cancel();
				return;
			}
			submission.formData.set('confirm', 'yes');
		}
		return submit(submission);
	};

	const pending = $derived(
		form && 'confirmDelete' in form ? data.groups.find((group) => group.id === form.confirmDelete) : undefined
	);
	const groupMessage = $derived(form?.action === 'group' && 'message' in form ? form : null);
</script>

<svelte:head>
	<title>Settings · {data.project.name}</title>
</svelte:head>

<!-- Monitor groups, then the member forms (moved unchanged from the old
     project page). -->
<div class="space-y-6">
	<div class="card p-5 sm:p-6">
		<h2 class="text-lg font-light text-ink">Monitor groups</h2>
		<p class="mt-1 text-sm text-muted">
			Groups arrange the monitor list, lowest position first. Deleting a group keeps its monitors, ungrouped.
		</p>

		{#if data.groupsError}
			<div role="alert" class="alert alert-danger mt-4">
				{data.groupsError}
			</div>
		{/if}
		{#if groupMessage?.message}
			<div
				role={groupMessage.success ? 'status' : 'alert'}
				class="mt-4 alert {groupMessage.success ? 'alert-success' : 'alert-danger'}"
			>
				{groupMessage.message}
			</div>
		{/if}
		<!-- The second step of a delete without JavaScript (see the deleteGroup action). -->
		{#if pending}
			<form
				method="POST"
				action="?/deleteGroup"
				use:enhance={submit}
				class="alert alert-danger mt-4 flex flex-wrap items-center gap-3"
			>
				<input type="hidden" name="group_id" value={pending.id} />
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">Delete group "{pending.name}"? Its monitors stay, ungrouped.</span>
				<button type="submit" disabled={busy} class="btn btn-danger-solid btn-sm">Yes, delete</button>
				<a href="?" class="text-ink-2 hover:text-ink">Cancel</a>
			</form>
		{/if}

		{#if data.groups.length > 0}
			<ul class="mt-4 space-y-2">
				{#each data.groups as group (group.id)}
					<li>
						<form
							method="POST"
							action="?/updateGroup"
							use:enhance={submitRow}
							class="grid gap-2 sm:grid-cols-[1fr_7rem_auto_auto] sm:items-center"
						>
							<input type="hidden" name="group_id" value={group.id} />
							<input name="name" type="text" required maxlength="200" value={group.name} aria-label="Group name" class={input} />
							<input name="position" type="number" min="0" max="1000000" step="1" value={group.position} aria-label="Position" class={input} />
							<button type="submit" disabled={busy} class="btn btn-secondary btn-sm">
								Save
							</button>
							<button
								type="submit"
								formaction="?/deleteGroup"
								disabled={busy}
								class="btn btn-danger btn-sm"
							>
								Delete
							</button>
						</form>
					</li>
				{/each}
			</ul>
		{:else if !data.groupsError}
			<p class="mt-4 text-sm text-muted">No groups yet.</p>
		{/if}

		<form method="POST" action="?/createGroup" use:enhance={submitCreate} class="mt-4 grid gap-2 sm:grid-cols-[1fr_7rem_auto] sm:items-end">
			<div>
				<label for="group-name" class="label">New group</label>
				<input id="group-name" name="name" type="text" required maxlength="200" placeholder="APIs" class={input} />
			</div>
			<div>
				<label for="group-position" class="label">Position</label>
				<input id="group-position" name="position" type="number" min="0" max="1000000" step="1" placeholder="0" class={input} />
			</div>
			<button type="submit" disabled={busy} class="btn btn-primary">Add group</button>
		</form>
	</div>

	<p class="text-sm text-muted">
		This project has no visible member list yet — add or remove members by their user ID.
	</p>

	<div class="grid gap-6 lg:grid-cols-2">
		<div class="card p-5 sm:p-6">
			<h2 class="text-lg font-light text-ink">Add member</h2>

			{#if form?.action === 'add'}
				<div role={form.success ? 'status' : 'alert'} class="mt-4 alert {form.success ? 'alert-success' : 'alert-danger'}">
					{form.message}
				</div>
			{/if}

			<form method="POST" action="?/addMember" class="mt-4 space-y-4">
				<div>
					<label for="add-user-id" class="label">User ID</label>
					<input
						id="add-user-id"
						name="user_id"
						type="text"
						placeholder="User ID (UUID)"
						required
						class="input"
					/>
					<p class="hint">
						Ask the user for their ID — it's shown on their Dashboard page.
					</p>
				</div>
				<div>
					<label for="project_role" class="label">Role</label>
					<select id="project_role" name="project_role" class="input">
						<option value="admin">admin</option>
						<option value="member" selected>member</option>
						<option value="viewer">viewer</option>
					</select>
				</div>
				<button type="submit" class="btn btn-primary">Add member</button>
			</form>
		</div>

		<div class="card p-5 sm:p-6">
			<h2 class="text-lg font-light text-ink">Remove member</h2>

			{#if form?.action === 'remove'}
				<div role={form.success ? 'status' : 'alert'} class="mt-4 alert {form.success ? 'alert-success' : 'alert-danger'}">
					{form.message}
				</div>
			{/if}

			<form method="POST" action="?/removeMember" class="mt-4 space-y-4">
				<div>
					<label for="remove-user-id" class="label">User ID</label>
					<input
						id="remove-user-id"
						name="user_id"
						type="text"
						placeholder="User ID (UUID)"
						required
						class="input"
					/>
					<p class="hint">
						Ask the user for their ID — it's shown on their Dashboard page.
					</p>
				</div>
				<button type="submit" class="btn btn-danger">Remove member</button>
			</form>
		</div>
	</div>
</div>
