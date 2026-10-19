import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { fakeEvent, MONITOR_ID, PROJECT_ID } from '#lib/server/core/fake-event';
import type { ImportReport } from '#lib/uptime/uptime';
import { actions, load } from './+page.server';
import Page from './+page.svelte';

const report: ImportReport = {
	dry_run: true,
	created: 1,
	skipped: 0,
	errors: 0,
	results: [{ line: 1, status: 'created', name: 'a.test', target: 'https://a.test/' }]
};
const fields = { format: 'urls', content: 'https://a.test/', skip_existing: 'on' };

describe('import actions', () => {
	it('previews with dry_run and imports without it, posting JSON to the engine', async () => {
		const { event, calls } = fakeEvent({ body: JSON.stringify(report), form: fields });
		expect(await actions.preview(event)).toEqual({ report, format: 'urls', content: 'https://a.test/' });
		expect(calls()).toEqual([
			{
				method: 'POST',
				path: `/projects/${PROJECT_ID}/monitors/import`,
				search: '',
				body: JSON.stringify({ format: 'urls', content: 'https://a.test/', defaults: {}, skip_existing: true, dry_run: true })
			}
		]);

		const real = fakeEvent({ body: JSON.stringify({ ...report, dry_run: false }), form: fields });
		await actions.import(real.event);
		expect(JSON.parse(real.calls()[0].body!)).toMatchObject({ dry_run: false });
	});

	it.each([
		[409, 'Another import is already running'],
		[413, 'too large'],
		[403, 'Only members can import']
	])('shows a friendly message for %i and keeps the status and the text', async (status, text) => {
		const { event } = fakeEvent({ status, body: JSON.stringify({ error: 'engine words' }), form: fields });
		const result: any = await actions.preview(event);
		expect(result.status).toBe(status);
		expect(result.data.message).toContain(text);
		expect(result.data.content).toBe('https://a.test/');
	});

	it('passes the engine message through for a 400', async () => {
		const { event } = fakeEvent({ status: 400, body: JSON.stringify({ error: 'at most 500 entries per import, got 501' }), form: fields });
		const result: any = await actions.import(event);
		expect(result.status).toBe(400);
		expect(result.data.message).toBe('at most 500 entries per import, got 501');
	});

	it('redirects to the login page on 401', async () => {
		const { event } = fakeEvent({ status: 401, form: fields });
		await expect(actions.preview(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('never calls the engine when the form is invalid', async () => {
		const { event, calls } = fakeEvent({ form: { format: 'urls', content: '' } });
		const result: any = await actions.preview(event);
		expect(result.status).toBe(400);
		expect(calls()).toEqual([]);
	});

	it('loads this project\'s groups, and none when that fails', async () => {
		const ok = fakeEvent({ body: JSON.stringify([{ id: 'g', name: 'Prod' }]) });
		expect(await load(ok.event)).toEqual({ groups: [{ id: 'g', name: 'Prod' }] });
		expect(ok.calls()[0].path).toBe(`/projects/${PROJECT_ID}/monitor-groups`);
		expect(await load(fakeEvent({ status: 500 }).event)).toEqual({ groups: [] });
	});
});

describe('import page', () => {
	const data = { project: { id: PROJECT_ID, name: 'P', slug: 'p' }, groups: [] };

	it('labels a preview "will be created" and links nothing', () => {
		const { body } = render(Page, { props: { data, form: { report, format: 'urls', content: '' } } } as any);
		expect(body).toContain('Will be created');
		expect(body).not.toContain(`/monitors/${MONITOR_ID}`);
	});

	it('links created monitors after a real import', () => {
		const done: ImportReport = {
			...report,
			dry_run: false,
			results: [{ line: 1, status: 'created', name: 'a.test', target: 'https://a.test/', monitor_id: MONITOR_ID }]
		};
		const { body } = render(Page, { props: { data, form: { report: done, format: 'urls', content: '' } } } as any);
		expect(body).toContain(`href="/projects/${PROJECT_ID}/monitors/${MONITOR_ID}"`);
	});

	it('escapes targets, errors and warnings, and shows the failure alert', () => {
		const evil = '<img src=x onerror=alert(1)>';
		const bad: ImportReport = {
			dry_run: true,
			created: 0,
			skipped: 0,
			errors: 1,
			results: [{ line: 2, status: 'error', target: evil, error: evil, warning: evil }]
		};
		const { body } = render(Page, { props: { data, form: { report: bad, format: 'urls', content: '' } } } as any);
		expect(body).not.toContain('<img');
		expect(body).toContain('&lt;img');
		const failed = render(Page, { props: { data, form: { message: 'Nope', format: 'urls', content: '' } } } as any).body;
		expect(failed).toContain('role="alert"');
	});

	it('is a plain multipart form with both actions as formaction', () => {
		const { body } = render(Page, { props: { data, form: null } } as any);
		expect(body).toContain('enctype="multipart/form-data"');
		expect(body).toContain('formaction="?/preview"');
		expect(body).toContain('formaction="?/import"');
	});
});
