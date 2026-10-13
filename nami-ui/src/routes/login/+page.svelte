<script lang="ts">
	import type { ActionData } from './$types';
	let { form }: { form: ActionData } = $props();
</script>

<form method="POST">
	<label>Email <input name="email" type="email" required /></label>
	<label>Password <input name="password" type="password" required /></label>
	<button type="submit">Log in</button>
</form>
{#if form?.message}
	<p>{form.message}</p>
{/if}
{#if form?.requiresTotp}
	<p>Enter your 2FA code to continue.</p>
{/if}
