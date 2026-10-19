<script lang="ts">
	import { enhance } from '$app/forms';
	import Card from '#lib/components/ui/Card.svelte';
	import PageHeader from '#lib/components/ui/PageHeader.svelte';
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

<svelte:head>
	<title>Security · nami</title>
</svelte:head>

<div class="space-y-6">
	<PageHeader title="Security" subtitle="Manage two-factor authentication for your account." />

	<Card title="Two-factor authentication" class="max-w-2xl">
		{#if form?.step === 'setup' && form?.error}
			<p role="alert" class="alert alert-danger mb-4">
				{form.error}
			</p>
		{/if}

		<form method="POST" action="?/setup" use:enhance>
			<button type="submit" class="btn btn-primary">Set up 2FA</button>
		</form>

		{#if provisioningUri}
			<div class="mt-5">
				<p class="text-sm text-ink-2">
					Scan this in your authenticator app, or paste the URL manually, then enter the 6-digit
					code below.
				</p>
				<code class="code mt-2 block break-all">
					{provisioningUri}
				</code>
			</div>

			<form method="POST" action="?/enable" use:enhance class="mt-5 max-w-xs space-y-3">
				<div>
					<label for="code" class="label">Authenticator code</label>
					<input
						id="code"
						name="code"
						type="text"
						inputmode="numeric"
						maxlength="6"
						placeholder="123456"
						class="input font-mono tracking-[0.3em]"
					/>
				</div>

				{#if form?.step === 'enable'}
					{#if form.success}
						<p role="status" class="alert alert-success">Two-factor authentication enabled.</p>
					{:else}
						<p role="alert" class="alert alert-danger">
							{form.message}
						</p>
					{/if}
				{/if}

				<button type="submit" class="btn btn-primary">Enable 2FA</button>
			</form>
		{/if}
	</Card>
</div>
