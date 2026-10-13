<script lang="ts">
	let { value, label = 'Copy' }: { value: string; label?: string } = $props();

	let outcome = $state<'idle' | 'copied' | 'failed'>('idle');
	let timer: ReturnType<typeof setTimeout> | undefined;

	function show(next: 'copied' | 'failed') {
		outcome = next;
		clearTimeout(timer);
		timer = setTimeout(() => (outcome = 'idle'), 2000);
	}

	// Drop the pending reset when the button goes away.
	$effect(() => () => clearTimeout(timer));

	async function copy() {
		try {
			await navigator.clipboard.writeText(value);
			show('copied');
		} catch {
			// Clipboard API unavailable (e.g. plain-HTTP origin): say so, and the
			// code block next to this button stays click-to-select.
			show('failed');
		}
	}
</script>

<button
	type="button"
	onclick={copy}
	class="shrink-0 rounded-md border border-[#262626] px-2 py-1 text-xs text-neutral-400 transition-colors hover:border-teal-500/30 hover:text-neutral-100"
>
	{outcome === 'copied' ? 'Copied' : outcome === 'failed' ? 'Copy failed' : label}
</button>
<!-- A button whose text changes is not announced; a status region is. -->
<span role="status" class="sr-only">
	{outcome === 'copied' ? 'Copied to clipboard.' : outcome === 'failed' ? 'Copy failed: select the text and copy it.' : ''}
</span>
