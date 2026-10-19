<script lang="ts">
	import AuthCard from '#lib/components/ui/AuthCard.svelte';
	import type { ActionData } from './$types';
	let { form }: { form: ActionData } = $props();
</script>

<AuthCard title="Log in">
	{#if form?.message}
		<div role="alert" class="alert alert-danger mb-4">
			{form.message}
		</div>
	{/if}

	{#if form?.requiresTotp}
		<div role="status" class="alert alert-info mb-4">Enter your 2FA code to continue.</div>

		{#if form?.totpError}
			<div role="alert" class="alert alert-danger mb-4">
				{form.totpError}
			</div>
		{/if}

		<form method="POST" action="?/verifyTotp" class="flex flex-col gap-4">
			<div>
				<label for="code" class="label">Code</label>
				<input
					id="code"
					name="code"
					type="text"
					inputmode="numeric"
					autocomplete="one-time-code"
					maxlength="6"
					required
					placeholder="123456"
					class="input text-center font-mono text-lg tracking-[0.4em]"
				/>
			</div>
			<button type="submit" class="btn btn-primary w-full py-2.5">Verify</button>
		</form>
	{:else}
		<form method="POST" action="?/login" class="flex flex-col gap-4">
			<div>
				<label for="email" class="label">Email</label>
				<input id="email" name="email" type="email" required autocomplete="email" class="input" />
			</div>
			<div>
				<div class="mb-1.5 flex items-center justify-between">
					<label for="password" class="text-sm font-medium text-ink">Password</label>
					<a href="/forgot-password" class="link text-xs">Forgot password?</a>
				</div>
				<input
					id="password"
					name="password"
					type="password"
					required
					autocomplete="current-password"
					class="input"
				/>
			</div>
			<button type="submit" class="btn btn-primary w-full py-2.5">Log in</button>
		</form>

		<div class="my-6 flex items-center gap-3">
			<div class="h-px flex-1 bg-border"></div>
			<span class="text-xs text-muted">or continue with</span>
			<div class="h-px flex-1 bg-border"></div>
		</div>

		<div class="grid grid-cols-2 gap-2">
			<a href="/auth/oauth/google/start" class="btn btn-secondary w-full">Google</a>
			<a href="/auth/oauth/github/start" class="btn btn-secondary w-full">GitHub</a>
		</div>
	{/if}

	{#snippet footer()}
		{#if !form?.requiresTotp}
			Don't have an account? <a href="/register" class="link">Register</a>
		{/if}
	{/snippet}
</AuthCard>
