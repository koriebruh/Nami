import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const SOURCE = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

function engine(
	overrides: {
		search?: Response;
		histogram?: Response;
		sources?: Response;
		patterns?: Response;
		views?: Response;
	} = {}
) {
	return (url: URL) => {
		if (url.pathname.endsWith('/logs/patterns')) return overrides.patterns ?? json({ patterns: [] });
		if (url.pathname.endsWith('/log-views')) return overrides.views ?? json([]);
		if (url.pathname.endsWith('/logs/histogram'))
			return overrides.histogram ?? json({ step_seconds: 60, total: 0, buckets: [] });
		if (url.pathname.endsWith('/logs')) return overrides.search ?? json({ logs: [], next_cursor: 'c1' });
		return overrides.sources ?? json([{ id: SOURCE, name: 'api' }]);
	};
}

describe('explorer load', () => {
	it('turns the URL state into allow-listed engine parameters with a fixed window', async () => {
		const { event, calls } = fakeEvent({
			url: `http://ui.test/p?range=6h&level=error&source=${SOURCE}&q=disk&in_fields=1&col=a.b&live=1&evil=1&cursor=zzz&filter=${encodeURIComponent('{"path":"a","op":"gt","value":"5"}')}`,
			respond: engine()
		});
		const data: any = await load(event);
		expect(data.nextCursor).toBe('c1');
		expect(data.state.columns).toEqual(['a.b']);
		const [search, histogram, sources] = calls();
		expect(search.path).toBe(`/projects/${PROJECT_ID}/logs`);
		const params = new URLSearchParams(search.search);
		expect(params.getAll('level')).toEqual(['error']);
		expect(params.getAll('source_id')).toEqual([SOURCE]);
		expect(params.get('in_fields')).toBe('true');
		expect(params.get('filter')).toBe('{"path":"a","op":"gt","value":"5"}');
		expect(params.get('limit')).toBe('100');
		expect(params.get('from')).toBe(data.window.from);
		expect(params.has('evil') || params.has('cursor') || params.has('col') || params.has('live')).toBe(false);
		expect(histogram.path).toBe(`/projects/${PROJECT_ID}/logs/histogram`);
		expect(new URLSearchParams(histogram.search).has('limit')).toBe(false);
		expect(sources.path).toBe(`/projects/${PROJECT_ID}/sources`);
		expect(Date.parse(data.window.to) - Date.parse(data.window.from)).toBe(6 * 3600_000);
	});

	it('sends a lost session to the login page', async () => {
		const { event } = fakeEvent({ status: 401 });
		await expect(load(event)).rejects.toMatchObject({
			status: 303,
			location: '/login'
		});
	});

	it('answers 403 for a non-member', async () => {
		const { event } = fakeEvent({ status: 403, body: '{"error":"forbidden"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 403 });
	});

	it('shows a search error on the page instead of failing it', async () => {
		const { event } = fakeEvent({
			respond: engine({
				search: json({ error: 'a filter regex is not valid RE2 syntax' }, 400)
			})
		});
		const data: any = await load(event);
		expect(data.searchError).toBe('a filter regex is not valid RE2 syntax');
		expect(data.logs).toEqual([]);
		expect(data.histogram).toEqual({ step_seconds: 60, total: 0, buckets: [] });
	});

	it('tolerates a failed histogram or source list', async () => {
		const { event } = fakeEvent({
			respond: engine({ histogram: json({}, 500), sources: json({}, 500) })
		});
		const data: any = await load(event);
		expect(data.histogram).toBeNull();
		expect(data.sources).toEqual([]);
	});

	it('says why the histogram or the source list is missing', async () => {
		const { event } = fakeEvent({
			respond: engine({ histogram: json({ error: 'the log store is unavailable' }, 500), sources: json({}, 500) })
		});
		const data: any = await load(event);
		expect(data.histogramError).toBe('the log store is unavailable');
		expect(data.sourcesError).toBe('Could not load the sources.');
		expect(data.logs).toEqual([]);
		expect(data.nextCursor).toBe('c1');

		const fine = fakeEvent({ respond: engine() });
		const ok: any = await load(fine.event);
		expect([ok.histogramError, ok.sourcesError]).toEqual([null, null]);
	});
});

describe('explorer, part 2', () => {
	it('the patterns tab loads patterns instead of a results page', async () => {
		const { event, calls } = fakeEvent({
			url: 'http://ui.test/p?tab=patterns&level=error&pattern=5',
			respond: (url) => {
				if (url.pathname.endsWith('/logs/patterns')) return json({ patterns: [{ pattern_hash: '5', pattern: 'x <num>', count: 3, first_seen: 'a', last_seen: 'b' }] });
				if (url.pathname.endsWith('/log-views')) return json([]);
				return engine()(url);
			}
		});
		const data: any = await load(event);
		expect(data.patterns).toHaveLength(1);
		const paths = calls().map((c) => c.path);
		expect(paths).toContain(`/projects/${PROJECT_ID}/logs/patterns`);
		expect(paths).not.toContain(`/projects/${PROJECT_ID}/logs`);
		const patterns = calls().find((c) => c.path.endsWith('/logs/patterns'))!;
		const params = new URLSearchParams(patterns.search);
		expect(params.getAll('pattern_hash')).toEqual(['5']);
		expect(params.has('limit') || params.has('cursor')).toBe(false);
	});

	it('loads saved views, and a failed views call does not break the page', async () => {
		const views = [{ id: 'v1', name: 'Errors', query: { range: '1h' } }];
		const ok = fakeEvent({ url: 'http://ui.test/p', respond: (url) => (url.pathname.endsWith('/log-views') ? json(views) : engine()(url)) });
		expect(((await load(ok.event)) as any).views).toEqual(views);
		const down = fakeEvent({ url: 'http://ui.test/p', respond: (url) => (url.pathname.endsWith('/log-views') ? json({}, 500) : engine()(url)) });
		expect(((await load(down.event)) as any).views).toEqual([]);
	});

	it('saves the current state as a view, without live or tab', async () => {
		const state = 'range=24h&level=error&live=1&tab=patterns';
		const { event, calls } = fakeEvent({ form: { name: 'Errors', state }, respond: () => json({ id: 'v1' }, 201) });
		expect(await actions.saveView(event)).toEqual({ savedView: 'Errors' });
		expect(calls()[0]).toMatchObject({ method: 'POST', path: `/projects/${PROJECT_ID}/log-views` });
		expect(JSON.parse(calls()[0].body!)).toEqual({ name: 'Errors', query: { range: '24h', levels: ['error'] } });
	});

	it('updates and deletes views with checked ids and a confirmed delete', async () => {
		const VIEW = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
		const update = fakeEvent({ form: { view_id: VIEW, state: 'q=x' }, respond: () => json({ id: VIEW }) });
		expect(await actions.updateView(update.event)).toEqual({ updatedView: true });
		expect(update.calls()[0]).toMatchObject({ method: 'PATCH', path: `/projects/${PROJECT_ID}/log-views/${VIEW}` });
		const bare = fakeEvent({ form: { view_id: VIEW } });
		expect(await actions.deleteView(bare.event)).toEqual({ confirmDeleteView: VIEW });
		expect(bare.calls()).toHaveLength(0);
		// `new Response('', { status: 204 })` throws (204 is a null-body status): answer with a null body.
		const confirmed = fakeEvent({ form: { view_id: VIEW, confirm: 'yes' }, respond: () => new Response(null, { status: 204 }) });
		expect(await actions.deleteView(confirmed.event)).toEqual({ deletedView: true });
		const forged = fakeEvent({ form: { view_id: '../../sources', confirm: 'yes' } });
		expect(await actions.deleteView(forged.event)).toMatchObject({ status: 400 });
		expect(forged.calls()).toHaveLength(0);
	});

	it('opens a view only on an explicit Open, relative presets staying relative', async () => {
		const VIEW = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
		const open = fakeEvent({
			url: `http://ui.test/projects/${PROJECT_ID}/logs?range=24h&/openView`,
			form: { view_id: VIEW, state: 'range=24h' },
			respond: () => json({ id: VIEW, name: 'Errors', query: { range: '6h', levels: ['error'] } })
		});
		await expect(actions.openView(open.event)).rejects.toMatchObject({
			status: 303,
			location: `/projects/${PROJECT_ID}/logs?range=6h&level=error`
		});
		expect(open.calls()[0]).toMatchObject({ method: 'GET', path: `/projects/${PROJECT_ID}/log-views/${VIEW}` });
		const gone = fakeEvent({ form: { view_id: VIEW }, respond: () => json({ error: 'not found' }, 404) });
		expect(await actions.openView(gone.event)).toMatchObject({ status: 404, data: { message: 'That view no longer exists.' } });
		const none = fakeEvent({ form: { view_id: '' } });
		expect(await actions.openView(none.event)).toMatchObject({ status: 400 });
		expect(none.calls()).toHaveLength(0);
	});

	it('tells a viewer that views are for members', async () => {
		const VIEW = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
		const forbidden = () => json({ error: 'forbidden' }, 403);
		const message = 'Only project members and admins can save, update or delete views.';
		const save = fakeEvent({ form: { name: 'Errors', state: '' }, respond: forbidden });
		expect(await actions.saveView(save.event)).toMatchObject({ status: 403, data: { message } });
		const update = fakeEvent({ form: { view_id: VIEW, state: '' }, respond: forbidden });
		expect(await actions.updateView(update.event)).toMatchObject({ status: 403, data: { message } });
		const remove = fakeEvent({ form: { view_id: VIEW, confirm: 'yes' }, respond: forbidden });
		expect(await actions.deleteView(remove.event)).toMatchObject({ status: 403, data: { message } });
	});

	it('shows a failed patterns call as an error, not as no patterns', async () => {
		const { event } = fakeEvent({
			url: 'http://ui.test/p?tab=patterns',
			respond: engine({ patterns: json({ error: 'too many queries' }, 429) })
		});
		const data: any = await load(event);
		expect(data.patternsError).toBe('too many queries');
		expect(data.patterns).toEqual([]);
	});
});
