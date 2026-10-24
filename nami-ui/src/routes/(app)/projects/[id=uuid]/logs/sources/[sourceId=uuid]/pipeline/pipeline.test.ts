import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const SOURCE = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
const PATH = `/projects/${PROJECT_ID}/sources/${SOURCE}`;

describe('pipeline page', () => {
	it('loads the source and its rules', async () => {
		const rules = [{ type: 'sample', n: 2 }];
		const { event } = fakeEvent({
			params: { sourceId: SOURCE },
			respond: (url) => (url.pathname.endsWith('/pipeline') ? json({ rules, updated_at: 't' }) : json({ id: SOURCE, name: 'api' }))
		});
		expect(await load(event)).toMatchObject({ source: { name: 'api' }, rules });
	});

	it('saves the rules as JSON and shows the engine error beside the editor', async () => {
		const ok = fakeEvent({ params: { sourceId: SOURCE }, form: { rules: '[{"type":"sample","n":2}]' }, respond: () => json({ rules: [], updated_at: 't' }) });
		expect(await actions.save(ok.event)).toEqual({ saved: true });
		expect(ok.calls()[0]).toMatchObject({ method: 'PUT', path: `${PATH}/pipeline`, body: '{"rules":[{"type":"sample","n":2}]}' });

		const bad = fakeEvent({ params: { sourceId: SOURCE }, form: { rules: '[]' }, respond: () => json({ error: 'rules[0]: nope' }, 400) });
		expect(await actions.save(bad.event)).toMatchObject({ status: 400, data: { message: 'rules[0]: nope' } });

		const garbage = fakeEvent({ params: { sourceId: SOURCE }, form: { rules: '{nope' } });
		expect(await actions.save(garbage.event)).toMatchObject({ status: 400 });
		expect(garbage.calls()).toHaveLength(0);

		const viewer = fakeEvent({ params: { sourceId: SOURCE }, form: { rules: '[]' }, respond: () => json({ error: 'forbidden' }, 403) });
		expect(await actions.save(viewer.event)).toMatchObject({
			status: 403,
			data: { message: 'Only project members and admins can change the pipeline.' }
		});
	});
});
