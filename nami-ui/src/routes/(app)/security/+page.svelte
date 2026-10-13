<script lang="ts">
	import { enhance } from '$app/forms';
	import type { ActionData } from './$types';
	let { form }: { form: ActionData } = $props();

	// Persist the provisioning URI across the subsequent `enable` submit:
	// that action's result has no `provisioningUri` field, so deriving
	// visibility straight from `form` would hide the confirm form (and any
	// wrong-code error) right after the first attempt, blocking retry.
	let provisioningUri = $state<string | null>(null);
	$effect(() => {
		if (form?.step === 'setup' && form.provisioningUri) {
			provisioningUri = form.provisioningUri;
		}
	});
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h1 class="text-lg font-semibold text-neutral-100">Security</h1>
	<p class="mt-1 text-sm text-neutral-500">Manage two-factor authentication for your account.</p>

	<div class="mt-6">
		<h2 class="text-sm font-semibold text-neutral-200">Two-factor authentication</h2>

		{#if form?.step === 'setup' && form?.error}
			<p class="mt-3 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				{form.error}
			</p>
		{/if}

		<form method="POST" action="?/setup" use:enhance class="mt-3">
			<button
				type="submit"
				class="inline-flex rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
			>
				Set up 2FA
			</button>
		</form>

		{#if provisioningUri}
			<div class="mt-4">
				<p class="text-sm text-neutral-400">
					Scan this in your authenticator app, or paste the URL manually, then enter the 6-digit
					code below.
				</p>
				<code
					class="mt-2 block break-all rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-xs text-neutral-400"
				>
					{provisioningUri}
				</code>
			</div>

			<form method="POST" action="?/enable" use:enhance class="mt-4 max-w-xs space-y-3">
				<div>
					<label for="code" class="mb-1.5 block text-sm font-medium text-neutral-300">
						Authenticator code
					</label>
					<input
						id="code"
						name="code"
						type="text"
						inputmode="numeric"
						maxlength="6"
						placeholder="123456"
						class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
					/>
				</div>

				{#if form?.step === 'enable'}
					{#if form.success}
						<p class="rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-2 text-sm text-teal-300">
							Two-factor authentication enabled.
						</p>
					{:else}
						<p class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
							{form.message}
						</p>
					{/if}
				{/if}

				<button
					type="submit"
					class="inline-flex rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
				>
					Enable 2FA
				</button>
			</form>
		{/if}
	</div>
</div>
