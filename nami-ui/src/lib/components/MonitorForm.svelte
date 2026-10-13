<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import type { CheckType, Monitor } from '#lib/uptime';

	let {
		monitor = null,
		submitLabel,
		message = null,
		cancelHref
	}: {
		monitor?: Monitor | null;
		submitLabel: string;
		message?: string | null;
		cancelHref: string;
	} = $props();

	// Read once, in a function, on purpose: after mount the select drives
	// which fields render, independent of later prop updates.
	function initialCheckType(): CheckType {
		return monitor?.check_type ?? 'http';
	}
	let checkType = $state<CheckType>(initialCheckType());

	// The submit button stays disabled from the submit until the result is
	// applied, so a double click cannot send the form twice (two monitors).
	// The button is in the `actions` snippet below on purpose: Svelte runs all
	// of a template's dynamic attributes in one effect, and the first re-run
	// of that effect after hydration rewrites every `value={...}` input with
	// its initial value, wiping what the user typed. As a snippet, the button
	// (and `submitLabel`, `cancelHref`) re-render without touching the inputs.
	let submitting = $state(false);
	const submit: SubmitFunction = () => {
		submitting = true;
		return async ({ update }) => {
			try {
				await update();
			} finally {
				submitting = false;
			}
		};
	};

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
</script>

<!-- use:enhance keeps the typed values and the selected check type on a
     validation error (the action returns fail(), so the form is not reset). -->
<form method="POST" use:enhance={submit} class="space-y-5">
	{#if message}
		<div role="alert" class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
			{message}
		</div>
	{/if}

	<div class="grid gap-4 sm:grid-cols-2">
		<div>
			<label for="name" class={label}>Name</label>
			<input id="name" name="name" type="text" required maxlength="200" value={monitor?.name ?? ''} class={input} />
		</div>
		<div>
			<label for="check_type" class={label}>Check type</label>
			<select id="check_type" name="check_type" bind:value={checkType} class={input}>
				<option value="http">HTTP status</option>
				<option value="keyword">Keyword on page</option>
				<option value="tcp">TCP port</option>
			</select>
		</div>
	</div>

	<!-- The maxlength values mirror the engine's size caps (url 2048 bytes,
	     host 253 characters, keyword 1024 bytes). The engine stays the
	     authority: it counts bytes, the browser counts UTF-16 units. -->
	{#if checkType === 'tcp'}
		<div class="grid gap-4 sm:grid-cols-[1fr_8rem]">
			<div>
				<label for="host" class={label}>Host</label>
				<input id="host" name="host" type="text" required maxlength="253" placeholder="db.internal" value={monitor?.host ?? ''} class={input} />
			</div>
			<div>
				<label for="port" class={label}>Port</label>
				<input id="port" name="port" type="number" required min="1" max="65535" value={monitor?.port ?? ''} class={input} />
			</div>
		</div>
	{:else}
		<div>
			<label for="url" class={label}>URL</label>
			<input id="url" name="url" type="url" required maxlength="2048" placeholder="https://example.com/health" value={monitor?.url ?? ''} class={input} />
		</div>
		{#if checkType === 'keyword'}
			<!-- Keyword checks need a body to search, so they always use GET. -->
			<input type="hidden" name="http_method" value="GET" />
			<div class="grid gap-4 sm:grid-cols-2">
				<div>
					<label for="keyword" class={label}>Keyword (case-sensitive)</label>
					<input id="keyword" name="keyword" type="text" required maxlength="1024" value={monitor?.keyword ?? ''} class={input} />
				</div>
				<div>
					<label for="keyword_mode" class={label}>Up when the page</label>
					<select id="keyword_mode" name="keyword_mode" value={monitor?.keyword_mode ?? 'contains'} class={input}>
						<option value="contains">contains the keyword</option>
						<option value="not_contains">does not contain the keyword</option>
					</select>
				</div>
			</div>
		{:else}
			<div>
				<label for="http_method" class={label}>Method</label>
				<select id="http_method" name="http_method" value={monitor?.http_method ?? 'GET'} class={input}>
					<option value="GET">GET</option>
					<option value="HEAD">HEAD</option>
				</select>
			</div>
		{/if}
		<div class="grid gap-4 sm:grid-cols-2">
			<div>
				<label for="expected_status_min" class={label}>Lowest OK status</label>
				<input id="expected_status_min" name="expected_status_min" type="number" min="100" max="599" value={monitor?.expected_status_min ?? 200} class={input} />
			</div>
			<div>
				<label for="expected_status_max" class={label}>Highest OK status</label>
				<input id="expected_status_max" name="expected_status_max" type="number" min="100" max="599" value={monitor?.expected_status_max ?? 399} class={input} />
			</div>
		</div>
	{/if}

	<div class="grid gap-4 sm:grid-cols-3">
		<div>
			<label for="interval_seconds" class={label}>Check every (seconds)</label>
			<input id="interval_seconds" name="interval_seconds" type="number" min="30" value={monitor?.interval_seconds ?? 60} class={input} />
		</div>
		<div>
			<label for="timeout_seconds" class={label}>Timeout (seconds)</label>
			<input id="timeout_seconds" name="timeout_seconds" type="number" min="1" max="60" aria-describedby="timeout-hint" value={monitor?.timeout_seconds ?? 10} class={input} />
		</div>
		<div>
			<label for="confirmation_threshold" class={label}>Failures before alerting</label>
			<input id="confirmation_threshold" name="confirmation_threshold" type="number" min="1" max="10" value={monitor?.confirmation_threshold ?? 2} class={input} />
		</div>
		<!-- The engine rejects a timeout that is not shorter than the interval. -->
		<p id="timeout-hint" class="text-xs text-neutral-500 sm:col-span-3">Timeout must be less than the interval.</p>
	</div>

	{@render actions()}
</form>

{#snippet actions()}
	<div class="flex items-center gap-3">
		<button
			type="submit"
			disabled={submitting}
			class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
		>
			{submitLabel}
		</button>
		<a href={cancelHref} class="text-sm text-neutral-500 hover:text-neutral-300">Cancel</a>
	</div>
{/snippet}
