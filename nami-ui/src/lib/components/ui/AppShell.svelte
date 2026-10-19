<script lang="ts">
	// The signed-in frame: a narrow icon sidebar (labels as tooltips; the
	// hamburger widens it on desktop and opens it as a drawer on mobile) and a
	// top bar with the current context, the incidents bell and the user menu.
	// Inside a project the sidebar adds the project's sections.
	import type { Snippet } from 'svelte';
	import { page } from '$app/state';
	import Brand from './Brand.svelte';
	import Icon, { type IconName } from './Icon.svelte';

	let {
		user,
		children
	}: {
		user: { email: string; global_role: string };
		children: Snippet;
	} = $props();

	type NavItem = { href: string; label: string; icon: IconName; active: boolean };

	const pathname = $derived(page.url.pathname);
	const project = $derived(page.data.project as { id: string; name: string; slug: string } | undefined);

	const mainItems = $derived<NavItem[]>([
		{ href: '/', label: 'Dashboard', icon: 'dashboard', active: pathname === '/' },
		{ href: '/projects', label: 'Projects', icon: 'folder', active: pathname === '/projects' }
	]);

	const projectItems = $derived.by<NavItem[]>(() => {
		if (!project) return [];
		const base = `/projects/${project.id}`;
		const sections: [string, string, IconName][] = [
			['monitors', 'Monitors', 'monitor'],
			['heartbeats', 'Heartbeats', 'heartbeat'],
			['incidents', 'Incidents', 'incident'],
			['logs', 'Logs', 'logs'],
			['maintenance', 'Maintenance', 'maintenance'],
			['settings', 'Settings', 'settings']
		];
		return sections.map(([slug, label, icon]) => ({
			href: `${base}/${slug}`,
			label,
			icon,
			active: pathname.startsWith(`${base}/${slug}`)
		}));
	});

	const bottomItems = $derived<NavItem[]>([
		{ href: '/security', label: 'Security', icon: 'shield', active: pathname === '/security' }
	]);

	let expanded = $state(false);
	let drawerOpen = $state(false);
	let userMenuOpen = $state(false);

	// A navigation closes the drawer and the user menu.
	$effect(() => {
		void pathname;
		drawerOpen = false;
		userMenuOpen = false;
	});

	function toggle() {
		if (window.matchMedia('(min-width: 768px)').matches) expanded = !expanded;
		else drawerOpen = !drawerOpen;
	}

	function onkeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') {
			drawerOpen = false;
			userMenuOpen = false;
		}
	}

	const initials = $derived(user.email.slice(0, 2).toUpperCase());
	const displayName = $derived(user.email.split('@')[0]);
</script>

<svelte:window {onkeydown} />

{#snippet navLink(item: NavItem, showLabel: boolean)}
	<a
		href={item.href}
		aria-current={item.active ? 'page' : undefined}
		class="group relative flex h-10 items-center gap-3 rounded-control px-3 text-sm transition-colors {item.active
			? 'bg-primary-soft text-primary'
			: 'text-faint hover:bg-surface hover:text-primary'}"
	>
		{#if item.active}
			<span class="absolute top-2 bottom-2 -left-3 w-0.5 rounded-full bg-primary" aria-hidden="true"></span>
		{/if}
		<Icon name={item.icon} size={20} />
		<span class={showLabel ? 'truncate font-medium text-ink-2 group-hover:text-primary group-aria-[current=page]:text-primary' : 'sr-only'}
			>{item.label}</span
		>
		{#if !showLabel}
			<!-- Tooltip for sighted mouse and keyboard users; the sr-only label
			     above is the accessible name. -->
			<span
				aria-hidden="true"
				class="pointer-events-none absolute top-1/2 left-full z-40 ml-3 -translate-y-1/2 rounded-control bg-ink px-2 py-1 text-xs font-medium whitespace-nowrap text-white opacity-0 shadow-pop transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100"
				>{item.label}</span
			>
		{/if}
	</a>
{/snippet}

{#snippet nav(showLabel: boolean)}
	<!-- Collapsed, the nav must not clip: its tooltips overflow to the right. -->
	<nav aria-label="Main" class="flex flex-1 flex-col gap-1 px-3 py-2 {showLabel ? 'overflow-y-auto' : ''}">
		{#each mainItems as item (item.href)}{@render navLink(item, showLabel)}{/each}
		{#if project}
			<div class="my-2 border-t border-border"></div>
			{#if showLabel}
				<p class="truncate px-3 pb-1 text-[11px] font-medium tracking-wider text-muted uppercase">{project.name}</p>
			{/if}
			{#each projectItems as item (item.href)}{@render navLink(item, showLabel)}{/each}
		{/if}
		<div class="mt-auto pt-2">
			{#each bottomItems as item (item.href)}{@render navLink(item, showLabel)}{/each}
		</div>
	</nav>
{/snippet}

<div class="min-h-screen bg-canvas text-ink">
	<a
		href="#main"
		class="sr-only rounded-control bg-primary px-3 py-2 text-sm text-white focus:not-sr-only focus:fixed focus:top-3 focus:left-3 focus:z-50"
		>Skip to content</a
	>

	<!-- Desktop sidebar -->
	<aside
		class="fixed inset-y-0 left-0 z-30 hidden flex-col border-r border-border bg-canvas transition-[width] md:flex {expanded
			? 'w-56'
			: 'w-[72px]'}"
	>
		<a href="/" class="flex h-16 items-center px-[22px]" aria-label="nami home">
			<Brand compact={!expanded} />
		</a>
		{@render nav(expanded)}
	</aside>

	<!-- Mobile drawer -->
	{#if drawerOpen}
		<button
			type="button"
			class="fixed inset-0 z-40 bg-ink/30 md:hidden"
			aria-label="Close menu"
			onclick={() => (drawerOpen = false)}
		></button>
		<aside
			id="app-drawer"
			class="fixed inset-y-0 left-0 z-50 flex w-64 flex-col border-r border-border bg-canvas shadow-pop md:hidden"
		>
			<div class="flex h-16 items-center justify-between px-5">
				<a href="/" aria-label="nami home"><Brand /></a>
				<button type="button" class="btn btn-ghost px-2" aria-label="Close menu" onclick={() => (drawerOpen = false)}>
					<Icon name="close" />
				</button>
			</div>
			{@render nav(true)}
		</aside>
	{/if}

	<div class="transition-[padding] {expanded ? 'md:pl-56' : 'md:pl-[72px]'}">
		<header class="sticky top-0 z-20 border-b border-border/70 bg-canvas/90 backdrop-blur">
			<div class="mx-auto flex h-16 max-w-7xl items-center gap-3 px-4 sm:px-6 lg:px-8">
				<button
					type="button"
					class="btn btn-ghost -ml-2 px-2"
					aria-label="Toggle navigation"
					aria-controls="app-drawer"
					aria-expanded={drawerOpen || expanded}
					onclick={toggle}
				>
					<Icon name="menu" size={20} />
				</button>
				<a href="/" class="md:hidden" aria-label="nami home"><Brand compact /></a>

				<!-- Context: where am I. -->
				<nav aria-label="Breadcrumb" class="hidden min-w-0 items-center gap-1.5 text-sm sm:flex">
					<a href="/projects" class="text-muted hover:text-primary">Projects</a>
					{#if project}
						<Icon name="chevronRight" size={14} class="text-faint" />
						<a href="/projects/{project.id}" class="truncate font-medium text-ink hover:text-primary">{project.name}</a>
					{/if}
				</nav>

				<div class="ml-auto flex items-center gap-1 sm:gap-3">
					{#if project}
						<a
							href="/projects/{project.id}/incidents"
							class="flex h-9 w-9 items-center justify-center rounded-full text-ink-2 hover:bg-surface hover:text-primary"
							aria-label="Incidents"
							title="Incidents"
						>
							<Icon name="bell" size={19} />
						</a>
					{/if}

					<details class="relative" bind:open={userMenuOpen}>
						<summary
							class="flex cursor-pointer list-none items-center gap-2.5 rounded-full py-1 pr-1 pl-1 hover:bg-surface sm:pr-2 [&::-webkit-details-marker]:hidden"
							aria-label="Account menu"
						>
							<span
								class="flex h-8 w-8 items-center justify-center rounded-full bg-gradient-to-br from-primary to-accent-periwinkle text-xs font-semibold text-white"
								aria-hidden="true">{initials}</span
							>
							<span class="hidden max-w-48 truncate text-sm text-ink sm:block">{displayName}</span>
							<Icon name="chevronDown" size={16} class="hidden text-muted sm:block" />
						</summary>
						<div
							class="absolute right-0 z-30 mt-2 w-64 rounded-card border border-border bg-surface p-1 text-sm shadow-pop"
						>
							<div class="px-3 py-2.5">
								<p class="truncate font-medium text-ink">{user.email}</p>
								<span class="badge badge-primary mt-1">{user.global_role}</span>
							</div>
							<div class="my-1 border-t border-border"></div>
							<a href="/security" class="flex items-center gap-2 rounded-control px-3 py-2 text-ink-2 hover:bg-primary-soft hover:text-primary">
								<Icon name="shield" size={16} /> Security
							</a>
							<form method="POST" action="/logout">
								<button
									type="submit"
									class="flex w-full items-center gap-2 rounded-control px-3 py-2 text-left text-ink-2 hover:bg-danger-soft hover:text-danger-ink"
								>
									<Icon name="logout" size={16} /> Log out
								</button>
							</form>
						</div>
					</details>
				</div>
			</div>
		</header>

		<main id="main" class="mx-auto max-w-7xl px-4 py-6 sm:px-6 sm:py-8 lg:px-8">
			{@render children()}
		</main>
	</div>
</div>
