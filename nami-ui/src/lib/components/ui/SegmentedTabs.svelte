<script lang="ts" generics="V extends string">
	// Segmented text tabs (the reference's Week / Month / Year). Two modes:
	// - links: `items` with `href` + `active`; plain links, so they work with
	//   URL params and without JavaScript; the active one has aria-current.
	// - buttons: `items` with `value`, plus `bind:value` and/or `onselect`;
	//   toggle buttons with aria-pressed, for client-side state.
	// In both, the active tab is outlined in primary; arrow keys, Home and
	// End move focus between tabs, and Tab leaves the group.
	type LinkItem = { href: string; label: string; active: boolean };
	type ButtonItem = { value: V; label: string; disabled?: boolean };

	let {
		items,
		label,
		size = 'md',
		value = $bindable(),
		onselect
	}: {
		items: LinkItem[] | ButtonItem[];
		label: string;
		size?: 'sm' | 'md';
		value?: V;
		onselect?: (value: V) => void;
	} = $props();

	const isLinks = (list: LinkItem[] | ButtonItem[]): list is LinkItem[] => list.length > 0 && 'href' in list[0];

	function onkeydown(event: KeyboardEvent) {
		if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
		const tabs = Array.from(
			(event.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('a, button:not([disabled])')
		);
		const index = tabs.indexOf(document.activeElement as HTMLElement);
		if (index === -1) return;
		event.preventDefault();
		const next =
			event.key === 'Home'
				? 0
				: event.key === 'End'
					? tabs.length - 1
					: (index + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length;
		tabs[next].focus();
	}

	function pick(next: V) {
		value = next;
		onselect?.(next);
	}

	const base = $derived(
		`rounded-control border whitespace-nowrap transition-colors ${size === 'sm' ? 'px-2.5 py-1 text-xs' : 'px-3.5 py-1.5 text-sm'}`
	);
	const on = 'border-primary text-primary';
	const off = 'border-transparent text-ink-2 hover:text-primary disabled:cursor-not-allowed disabled:opacity-50';
</script>

{#if isLinks(items)}
	<nav aria-label={label} class="-mx-1 max-w-full overflow-x-auto px-1 py-0.5">
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="flex items-center gap-1" {onkeydown}>
			{#each items as item (item.href)}
				<a
					href={item.href}
					aria-current={item.active ? 'page' : undefined}
					class="{base} {item.active ? on : off}">{item.label}</a
				>
			{/each}
		</div>
	</nav>
{:else}
	<div role="group" aria-label={label} class="-mx-1 max-w-full overflow-x-auto px-1 py-0.5">
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="flex items-center gap-1" {onkeydown}>
			{#each items as item (item.value)}
				<button
					type="button"
					aria-pressed={value === item.value}
					disabled={item.disabled}
					onclick={() => pick(item.value)}
					class="{base} {value === item.value ? on : off}">{item.label}</button
				>
			{/each}
		</div>
	</div>
{/if}
