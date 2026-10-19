import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { load } from './+page.server';

const SOURCE = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

function engine(
	overrides: {
		search?: Response;
		histogram?: Response;
		sources?: Response;
	} = {}
) {
	return (url: URL) => {
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
});
