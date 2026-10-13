<script lang="ts">
	import type { ActionData } from './$types';
	let { form }: { form: ActionData } = $props();
</script>

<form method="POST">
	<label>Email <input name="email" type="email" required /></label>
	<label>Password <input name="password" type="password" required minlength="8" /></label>
	<button type="submit">Register</button>
</form>
{#if form?.message}
	<p>{form.message}</p>
{/if}
