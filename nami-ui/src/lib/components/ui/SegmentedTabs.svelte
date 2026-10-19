<script lang="ts">
	// Segmented text tabs (the reference's Week / Month / Year): plain links,
	// so they work with URL params and without JavaScript. The active one is
	// outlined in primary and marked aria-current. Arrow keys, Home and End
	// move focus between them; Tab leaves the group.
	let {
		items,
		label,
		size = 'md'
	}: {
		items: { href: string; label: string; active: boolean }[];
		label: string;
		size?: 'sm' | 'md';
	} = $props();

	function onkeydown(event: KeyboardEvent) {
		const keys = ['ArrowLeft', 'ArrowRight', 'Home', 'End'];
		if (!keys.includes(event.key)) return;
		const links = Array.from((event.currentTarget as HTMLElement).querySelectorAll<HTMLAnchorElement>('a'));
		const index = links.indexOf(document.activeElement as HTMLAnchorElement);
		if (index === -1) return;
		event.preventDefault();
		const next =
			event.key === 'Home'
				? 0
				: event.key === 'End'
					? links.length - 1
					: (index + (event.key === 'ArrowRight' ? 1 : -1) + links.length) % links.length;
		links[next].focus();
	}
</script>

<nav aria-label={label} class="-mx-1 max-w-full overflow-x-auto px-1 py-0.5">
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="flex items-center gap-1" {onkeydown}>
		{#each items as item (item.href)}
			<a
				href={item.href}
				aria-current={item.active ? 'page' : undefined}
				class="rounded-control border whitespace-nowrap transition-colors {size === 'sm'
					? 'px-2.5 py-1 text-xs'
					: 'px-3.5 py-1.5 text-sm'} {item.active
					? 'border-primary text-primary'
					: 'border-transparent text-ink-2 hover:text-primary'}">{item.label}</a
			>
		{/each}
	</div>
</nav>
