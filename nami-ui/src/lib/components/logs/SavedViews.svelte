<script lang="ts">
	// Saved views: open, update or delete the picked one, or save the current
	// explorer state as a new one. Picking a view in the select does nothing
	// by itself (arrow keys must not navigate); "Open" does. All of it is
	// plain form posts, so it works without JavaScript too. The forms send
	// the explorer query string (`query`, as the field `state`); the action
	// turns it into a view query without `live` or `tab`.
	import { enhance, type SubmitFunction } from '$app/forms';
	import type { LogView } from '#lib/logs/logs';

	type ViewForm = {
		message?: string;
		savedView?: string;
		updatedView?: boolean;
		deletedView?: boolean;
		confirmDeleteView?: string;
	};

	let {
		views,
		path,
		query,
		form
	}: { views: LogView[]; path: string; query: string; form: ViewForm | null | undefined } = $props();

	const pending = $derived(form?.confirmDeleteView ? views.find((view) => view.id === form.confirmDeleteView) : undefined);
	// The forms keep the explorer state in their action URL, so a post without
	// JavaScript comes back to the same explorer.
	const action = (name: string) => `?${query ? `${query}&` : ''}/${name}`;

	let busy = $state(false);
	const submit =
		(reset: boolean): SubmitFunction =>
		(submission) => {
			if (submission.action.searchParams.has('/deleteView') && !submission.formData.has('confirm')) {
				const view = views.find((other) => other.id === submission.formData.get('view_id'));
				if (!confirm(`Delete view "${view?.name ?? ''}"?`)) {
					submission.cancel();
					return;
				}
				submission.formData.set('confirm', 'yes');
			}
			busy = true;
			return async ({ update }) => {
				try {
					await update({ reset });
				} finally {
					busy = false;
				}
			};
		};
</script>

<div class="flex flex-wrap items-center gap-2">
	<!-- Not reset after a post: the picked view stays picked. -->
	<form method="POST" action={action('openView')} use:enhance={submit(false)} class="flex flex-wrap items-center gap-1">
		<label class="sr-only" for="saved-view">Saved view</label>
		<select id="saved-view" name="view_id" required class="select w-auto py-1.5 text-sm">
			<option value="">{views.length ? 'Saved views…' : 'No saved views'}</option>
			{#each views as view (view.id)}
				<option value={view.id}>{view.name}</option>
			{/each}
		</select>
		<input type="hidden" name="state" value={query} />
		<button type="submit" disabled={busy} class="btn btn-secondary btn-sm">Open</button>
		<button type="submit" formaction={action('updateView')} disabled={busy} class="btn btn-secondary btn-sm">Update</button>
		<button type="submit" formaction={action('deleteView')} disabled={busy} class="btn btn-danger btn-sm">Delete</button>
	</form>
	<form method="POST" action={action('saveView')} use:enhance={submit(true)} class="flex items-center gap-1">
		<label class="sr-only" for="view-name">View name</label>
		<input id="view-name" name="name" required maxlength="200" placeholder="View name" class="input w-40 py-1 text-xs" />
		<input type="hidden" name="state" value={query} />
		<button type="submit" disabled={busy} class="btn btn-secondary btn-sm">Save as view</button>
	</form>
</div>
{#if form?.message}
	<p class="text-xs text-danger-ink" role="alert">{form.message}</p>
{:else if form?.savedView}
	<p class="text-xs text-success-ink" role="status">Saved view "{form.savedView}".</p>
{:else if form?.updatedView}
	<p class="text-xs text-success-ink" role="status">View updated.</p>
{:else if form?.deletedView}
	<p class="text-xs text-success-ink" role="status">View deleted.</p>
{/if}
<!-- The second step of a delete without JavaScript (see the deleteView action). -->
{#if pending}
	<form method="POST" action={action('deleteView')} use:enhance={submit(true)} class="alert alert-danger flex flex-wrap items-center gap-3">
		<input type="hidden" name="view_id" value={pending.id} />
		<input type="hidden" name="confirm" value="yes" />
		<span class="wrap-anywhere">Delete view "{pending.name}"?</span>
		<button type="submit" disabled={busy} class="btn btn-danger-solid btn-sm">Yes, delete</button>
		<a href={query ? `${path}?${query}` : path} class="text-ink-2 hover:text-ink">Cancel</a>
	</form>
{/if}
