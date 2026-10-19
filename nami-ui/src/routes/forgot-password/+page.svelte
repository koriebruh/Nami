<script lang="ts">
	import AuthCard from '#lib/components/ui/AuthCard.svelte';
	import type { ActionData } from './$types';
	let { form }: { form: ActionData } = $props();
</script>

<AuthCard title="Reset your password">
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
		<button type="submit" class="btn btn-primary w-full py-2.5">Send reset link</button>
	</form>

	{#snippet footer()}
		Remembered your password? <a href="/login" class="link">Log in</a>
	{/snippet}
</AuthCard>
