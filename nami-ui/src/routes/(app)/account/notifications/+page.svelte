<script lang="ts">
	import ChannelList from '#lib/components/ChannelList.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<svelte:head>
	<title>Notifications</title>
</svelte:head>

<div class="space-y-6">
	<div>
		<h1 class="text-2xl font-light text-ink">Notifications</h1>
		<p class="mt-1 text-sm text-muted">
			Your personal channels. When an escalation step targets you, Nami notifies every enabled channel here. Email
			goes to your account address, so it needs no channel.
		</p>
	</div>
	<ChannelList channels={data.channels} {form} scope="personal" canEdit={true} />
</div>
