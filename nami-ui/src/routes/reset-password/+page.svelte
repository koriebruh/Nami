<script lang="ts">
	import type { ActionData, PageData } from './$types';
	let { data, form }: { data: PageData; form: ActionData } = $props();
</script>

<div class="flex min-h-screen items-center justify-center px-4">
	<div class="w-full max-w-[400px] rounded-xl border border-[#262626] bg-[#141414] p-8">
		<div class="mb-6 flex flex-col gap-1">
			<span class="text-sm font-medium tracking-wide text-teal-400">nami</span>
			<h1 class="text-xl font-semibold text-neutral-100">Set a new password</h1>
			<p class="text-sm text-neutral-500">Monitor everything, self-hosted.</p>
		</div>

		{#if form?.success === false}
			<div class="mb-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				{form.message}
			</div>
		{/if}

		{#if !data.token}
			<div class="mb-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				This reset link is invalid or missing a token.
			</div>
		{:else}
			<form method="POST" class="flex flex-col gap-4">
				<input type="hidden" name="token" value={data.token} />
				<div>
					<label for="new_password" class="mb-1.5 block text-sm font-medium text-neutral-300"
						>New password</label
					>
					<input
						id="new_password"
						name="new_password"
						type="password"
						required
						minlength="8"
						autocomplete="new-password"
						class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
					/>
					<p class="mt-1.5 text-xs text-neutral-600">At least 8 characters.</p>
				</div>
				<button
					type="submit"
					class="w-full rounded-md bg-teal-500 px-3 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
				>
					Set new password
				</button>
			</form>
		{/if}

		<p class="mt-6 text-center text-sm text-neutral-500">
			<a href="/login" class="text-teal-400 hover:underline">Back to login</a>
		</p>
	</div>
</div>
