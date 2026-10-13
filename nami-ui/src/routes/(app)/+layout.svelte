<script lang="ts">
	import type { LayoutProps } from './$types';
	let { data, children }: LayoutProps = $props();
</script>

<div class="min-h-screen bg-[#0a0a0a] text-neutral-200">
	<header class="border-b border-[#262626] bg-[#141414]">
		<div class="mx-auto flex max-w-5xl items-center justify-between px-6 py-4">
			<nav class="flex items-center gap-6">
				<a href="/" class="text-sm font-medium tracking-wide text-teal-400">nami</a>
				<a href="/" class="text-sm text-neutral-400 hover:text-neutral-100">Dashboard</a>
				<a href="/projects" class="text-sm text-neutral-400 hover:text-neutral-100">Projects</a>
				<a href="/security" class="text-sm text-neutral-400 hover:text-neutral-100">Security</a>
			</nav>
			<div class="flex items-center gap-4">
				<span class="text-sm text-neutral-500">{data.user.email}</span>
				<span
					class="rounded-full border border-[#262626] bg-[#0a0a0a] px-2 py-0.5 text-xs text-neutral-400"
				>
					{data.user.global_role}
				</span>
				<form method="POST" action="/logout">
					<button
						type="submit"
						class="rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400"
					>
						Log out
					</button>
				</form>
			</div>
		</div>
	</header>

	<main class="mx-auto max-w-5xl px-6 py-8">
		{@render children()}
	</main>
</div>
