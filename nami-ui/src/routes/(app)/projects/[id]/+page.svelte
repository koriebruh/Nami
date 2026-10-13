<script lang="ts">
	import type { PageData, ActionData } from './$types';

	let { data, form }: { data: PageData; form: ActionData } = $props();
</script>

<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h1 class="text-lg font-semibold text-neutral-100">{data.project.name}</h1>
		<p class="mt-1 text-sm text-neutral-500">{data.project.slug}</p>

		<p class="mt-4 text-sm text-neutral-500">
			This project has no visible member list yet — add or remove members by their user ID.
		</p>
	</div>

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

	<a href="/projects" class="inline-block text-sm text-neutral-500 hover:text-neutral-300">← Back to projects</a>
</div>
