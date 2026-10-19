<script lang="ts">
	import AuthCard from '#lib/components/ui/AuthCard.svelte';
	import type { ActionData } from './$types';
	let { form }: { form: ActionData } = $props();
</script>

<AuthCard title="Create your account">
	{#if form?.message}
		<div role="status" class="alert alert-info mb-4">
			{form.message}
		</div>
	{/if}

	<form method="POST" class="flex flex-col gap-4">
		<div>
			<label for="email" class="label">Email</label>
			<input id="email" name="email" type="email" required autocomplete="email" class="input" />
		</div>
		<div>
			<label for="password" class="label">Password</label>
			<input
				id="password"
				name="password"
				type="password"
				required
				minlength="8"
				autocomplete="new-password"
				aria-describedby="password-hint"
				class="input"
			/>
			<p id="password-hint" class="hint">At least 8 characters.</p>
		</div>
		<button type="submit" class="btn btn-primary w-full py-2.5">Create account</button>
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

	{#snippet footer()}
		Already have an account? <a href="/login" class="link">Log in</a>
	{/snippet}
</AuthCard>
