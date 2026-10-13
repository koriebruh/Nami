<script lang="ts">
	import type { ActionData } from './$types';
	let { form }: { form: ActionData } = $props();
</script>

<div class="flex min-h-screen items-center justify-center px-4">
	<div class="w-full max-w-[400px] rounded-xl border border-[#262626] bg-[#141414] p-8">
		<div class="mb-6 flex flex-col gap-1">
			<span class="text-sm font-medium tracking-wide text-teal-400">nami</span>
			<h1 class="text-xl font-semibold text-neutral-100">Log in</h1>
			<p class="text-sm text-neutral-500">Monitor everything, self-hosted.</p>
		</div>

		{#if form?.message}
			<div class="mb-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				{form.message}
			</div>
		{/if}

		{#if form?.requiresTotp}
			<div class="mb-4 rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-2 text-sm text-teal-300">
				Enter your 2FA code to continue.
			</div>
		{/if}

		<form method="POST" class="flex flex-col gap-4">
			<div>
				<label for="email" class="mb-1.5 block text-sm font-medium text-neutral-300">Email</label>
				<input
					id="email"
					name="email"
					type="email"
					required
					autocomplete="email"
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				/>
			</div>
			<div>
				<label for="password" class="mb-1.5 block text-sm font-medium text-neutral-300">Password</label>
				<input
					id="password"
					name="password"
					type="password"
					required
					autocomplete="current-password"
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				/>
			</div>
			<button
				type="submit"
				class="w-full rounded-md bg-teal-500 px-3 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
			>
				Log in
			</button>
		</form>

		<p class="mt-6 text-center text-sm text-neutral-500">
			Don't have an account? <a href="/register" class="text-teal-400 hover:underline">Register</a>
		</p>
	</div>
</div>
