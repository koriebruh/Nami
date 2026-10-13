<script lang="ts">
	import type { PageProps } from './$types';
	let { data }: PageProps = $props();
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h1 class="text-lg font-semibold text-neutral-100">Dashboard</h1>
	<p class="mt-1 text-sm text-neutral-500">Signed in as {data.user.email}.</p>

	<dl class="mt-6 grid grid-cols-2 gap-4 text-sm sm:grid-cols-3">
		<div>
			<dt class="text-neutral-500">Email</dt>
			<dd class="mt-1 text-neutral-200">{data.user.email}</dd>
		</div>
		<div>
			<dt class="text-neutral-500">Global role</dt>
			<dd class="mt-1 text-neutral-200">{data.user.global_role}</dd>
		</div>
		<div>
			<dt class="text-neutral-500">User ID</dt>
			<dd class="mt-1 font-mono text-xs text-neutral-400">{data.user.id}</dd>
		</div>
	</dl>
</div>
