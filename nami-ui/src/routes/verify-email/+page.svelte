<script lang="ts">
	import { page } from '$app/state';
	import AuthCard from '#lib/components/ui/AuthCard.svelte';
	import type { ActionData, PageData } from './$types';
	let { data, form }: { data: PageData; form: ActionData } = $props();
	const token = $derived(page.url.searchParams.get('token') ?? '');
</script>

<AuthCard title="Verify your email">
	{#if form?.verified}
		<div role="status" class="alert alert-success mb-4">Your email is verified. You can log in now.</div>
		<a href="/login" class="btn btn-primary w-full py-2.5">Log in</a>
	{:else if form?.verified === false || !data.hasToken}
		<div role="alert" class="alert alert-danger mb-4">
			This verification link is invalid or has expired.
		</div>
	{:else}
		<form method="POST" class="flex flex-col gap-4">
			<input type="hidden" name="token" value={token} />
			<button type="submit" class="btn btn-primary w-full py-2.5">Confirm my email</button>
		</form>
	{/if}

	{#snippet footer()}
		<a href="/login" class="link">Back to login</a>
	{/snippet}
</AuthCard>
