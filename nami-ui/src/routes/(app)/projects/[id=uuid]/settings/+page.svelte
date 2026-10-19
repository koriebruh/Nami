<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';

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
	const outlineButton =
		'rounded-md border border-[#262626] px-3 py-2 text-sm transition-colors disabled:cursor-not-allowed disabled:opacity-50';
</script>

<svelte:head>
	<title>Settings · {data.project.name}</title>
</svelte:head>

<!-- Monitor groups, then the member forms (moved unchanged from the old
     project page). -->
<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-lg font-semibold text-neutral-100">Monitor groups</h2>
		<p class="mt-1 text-sm text-neutral-500">
			Groups arrange the monitor list, lowest position first. Deleting a group keeps its monitors, ungrouped.
		</p>

		{#if data.groupsError}
			<div role="alert" class="mt-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				{data.groupsError}
			</div>
		{/if}
		{#if groupMessage?.message}
			<div
				role={groupMessage.success ? 'status' : 'alert'}
				class="mt-4 rounded-md border px-3 py-2 text-sm {groupMessage.success
					? 'border-teal-500/30 bg-teal-500/10 text-teal-300'
					: 'border-red-500/30 bg-red-500/10 text-red-400'}"
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
				class="mt-4 flex flex-wrap items-center gap-3 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-300"
			>
				<input type="hidden" name="group_id" value={pending.id} />
				<input type="hidden" name="confirm" value="yes" />
				<span class="wrap-anywhere">Delete group "{pending.name}"? Its monitors stay, ungrouped.</span>
				<button type="submit" disabled={busy} class="{outlineButton} border-red-500/40 text-red-300">Yes, delete</button>
				<a href="?" class="text-neutral-400 hover:text-neutral-200">Cancel</a>
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
							<button type="submit" disabled={busy} class="{outlineButton} text-neutral-300 hover:border-teal-500/30 hover:text-neutral-100">
								Save
							</button>
							<button
								type="submit"
								formaction="?/deleteGroup"
								disabled={busy}
								class="{outlineButton} text-neutral-400 hover:border-red-500/30 hover:text-red-400"
							>
								Delete
							</button>
						</form>
					</li>
				{/each}
			</ul>
		{:else if !data.groupsError}
			<p class="mt-4 text-sm text-neutral-500">No groups yet.</p>
		{/if}

		<form method="POST" action="?/createGroup" use:enhance={submitCreate} class="mt-4 grid gap-2 sm:grid-cols-[1fr_7rem_auto] sm:items-end">
			<div>
				<label for="group-name" class="mb-1.5 block text-sm font-medium text-neutral-300">New group</label>
				<input id="group-name" name="name" type="text" required maxlength="200" placeholder="APIs" class={input} />
			</div>
			<div>
				<label for="group-position" class="mb-1.5 block text-sm font-medium text-neutral-300">Position</label>
				<input id="group-position" name="position" type="number" min="0" max="1000000" step="1" placeholder="0" class={input} />
			</div>
			<button
				type="submit"
				disabled={busy}
				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 disabled:cursor-not-allowed disabled:opacity-50"
			>
				Add group
			</button>
		</form>
	</div>

	<p class="text-sm text-neutral-500">
		This project has no visible member list yet — add or remove members by their user ID.
	</p>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-lg font-semibold text-neutral-100">Add member</h2>

		{#if form?.action === 'add'}
			<div
				class="mt-4 rounded-md border px-3 py-2 text-sm {form.success
					? 'border-teal-500/30 bg-teal-500/10 text-teal-300'
					: 'border-red-500/30 bg-red-500/10 text-red-400'}"
			>
				{form.message}
			</div>
		{/if}

		<form method="POST" action="?/addMember" class="mt-4 space-y-4">
			<div>
				<label for="add-user-id" class="mb-1.5 block text-sm font-medium text-neutral-300">User ID</label>
				<input
					id="add-user-id"
					name="user_id"
					type="text"
					placeholder="User ID (UUID)"
					required
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				/>
				<p class="mt-1 text-xs text-neutral-600">
					Ask the user for their ID — it's shown on their Dashboard page.
				</p>
			</div>
			<div>
				<label for="project_role" class="mb-1.5 block text-sm font-medium text-neutral-300">Role</label>
				<select
					id="project_role"
					name="project_role"
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				>
					<option value="admin">admin</option>
					<option value="member" selected>member</option>
					<option value="viewer">viewer</option>
				</select>
			</div>
			<button
				type="submit"
				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
			>
				Add member
			</button>
		</form>
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-lg font-semibold text-neutral-100">Remove member</h2>

		{#if form?.action === 'remove'}
			<div
				class="mt-4 rounded-md border px-3 py-2 text-sm {form.success
					? 'border-teal-500/30 bg-teal-500/10 text-teal-300'
					: 'border-red-500/30 bg-red-500/10 text-red-400'}"
			>
				{form.message}
			</div>
		{/if}

		<form method="POST" action="?/removeMember" class="mt-4 space-y-4">
			<div>
				<label for="remove-user-id" class="mb-1.5 block text-sm font-medium text-neutral-300">User ID</label>
				<input
					id="remove-user-id"
					name="user_id"
					type="text"
					placeholder="User ID (UUID)"
					required
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				/>
				<p class="mt-1 text-xs text-neutral-600">
					Ask the user for their ID — it's shown on their Dashboard page.
				</p>
			</div>
			<button
				type="submit"
				class="rounded-md border border-[#262626] px-4 py-2 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400"
			>
				Remove member
			</button>
		</form>
	</div>
</div>
