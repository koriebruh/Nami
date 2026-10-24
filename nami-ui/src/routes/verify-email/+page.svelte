<script lang="ts">
	import AuthCard from '#lib/components/ui/AuthCard.svelte';
	import type { PageData } from './$types';
	let { data }: { data: PageData } = $props();
</script>

<AuthCard title="Verify your email">
	{#if data.verified}
		<div role="status" class="alert alert-success mb-4">Your email is verified. You can log in now.</div>
		<a href="/login" class="btn btn-primary w-full py-2.5">Log in</a>
	{:else}
		<div role="alert" class="alert alert-danger mb-4">
			This verification link is invalid or has expired.
		</div>
	{/if}

	{#snippet footer()}
		<a href="/login" class="link">Back to login</a>
	{/snippet}
</AuthCard>
