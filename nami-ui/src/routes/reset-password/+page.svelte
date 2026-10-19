<script lang="ts">
	import AuthCard from '#lib/components/ui/AuthCard.svelte';
	import type { ActionData, PageData } from './$types';
	let { data, form }: { data: PageData; form: ActionData } = $props();
</script>

<AuthCard title="Set a new password">
	{#if form?.success === false}
		<div role="alert" class="alert alert-danger mb-4">
			{form.message}
		</div>
	{/if}

	{#if !data.token}
		<div role="alert" class="alert alert-danger mb-4">This reset link is invalid or missing a token.</div>
	{:else}
		<form method="POST" class="flex flex-col gap-4">
			<input type="hidden" name="token" value={data.token} />
			<div>
				<label for="new_password" class="label">New password</label>
				<input
					id="new_password"
					name="new_password"
					type="password"
					required
					minlength="8"
					autocomplete="new-password"
					aria-describedby="new-password-hint"
					class="input"
				/>
				<p id="new-password-hint" class="hint">At least 8 characters.</p>
			</div>
			<button type="submit" class="btn btn-primary w-full py-2.5">Set new password</button>
		</form>
	{/if}

	{#snippet footer()}
		<a href="/login" class="link">Back to login</a>
	{/snippet}
</AuthCard>
