import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { fakeEvent, MONITOR_ID, PROJECT_ID } from '#lib/server/core/fake-event';
import type { IncidentDetail } from '#lib/incidents/incidents';
import { actions, load } from './+page.server';
import Page from './+page.svelte';

const INCIDENT_ID = '3f0c8a52-7d1e-4b6a-9c2f-5e8d7a6b4c31';
const params = { incidentId: INCIDENT_ID };
const base = `/projects/${PROJECT_ID}/incidents/${INCIDENT_ID}`;

describe('incident detail', () => {
	it('loads the incident and severities, no-store', async () => {
		const { event, calls, setHeaders } = fakeEvent({ params, respond: (url) =>
			new Response(url.pathname.endsWith('/severities') ? '[]' : JSON.stringify({ id: INCIDENT_ID, events: [] })) });
		expect(await load(event)).toMatchObject({ incident: { id: INCIDENT_ID }, severities: [] });
		expect(calls().map((c) => c.path).sort()).toEqual([base, `/projects/${PROJECT_ID}/severities`].sort());
		expect(setHeaders).toHaveBeenCalledWith({ 'cache-control': 'private, no-store' });
	});

	it('404s for an unknown incident', async () => {
		await expect(load(fakeEvent({ params, status: 404, body: '{"error":"not found"}' }).event)).rejects.toMatchObject({ status: 404 });
	});

	it.each(['acknowledge', 'resolve', 'reopen'] as const)('%s posts {} to its engine action', async (name) => {
		const { event, calls } = fakeEvent({ params, form: {} });
		await actions[name](event);
		expect(calls()).toEqual([{ method: 'POST', path: `${base}/${name}`, search: '', body: '{}' }]);
	});

	it('shows a 409 from the engine as the form message', async () => {
		const { event } = fakeEvent({ params, form: {}, status: 409, body: '{"error":"incident is not open"}' });
		expect(await actions.acknowledge(event)).toMatchObject({ status: 409, data: { message: 'incident is not open' } });
	});

	it('posts comments and postmortems as JSON and refuses an empty comment locally', async () => {
		const { event, calls } = fakeEvent({ params, form: { body: ' rolled back ' }, status: 201 });
		await actions.comment(event);
		expect(calls()[0]).toMatchObject({ method: 'POST', path: `${base}/comments`, body: '{"body":"rolled back"}' });
		const empty = fakeEvent({ params, form: { body: '  ' } });
		expect(await actions.comment(empty.event)).toMatchObject({ status: 400 });
		expect(empty.calls()).toEqual([]);
		const pm = fakeEvent({ params, form: { postmortem: '' } });
		await actions.postmortem(pm.event);
		expect(pm.calls()[0]).toMatchObject({ method: 'PUT', path: `${base}/postmortem`, body: '{"postmortem":null}' });
	});

	it('refuses a severity id that is not a UUID before calling the engine', async () => {
		const { event, calls } = fakeEvent({ params, form: { severity_id: '../x' } });
		expect(await actions.severity(event)).toMatchObject({ status: 400 });
		expect(calls()).toEqual([]);
	});

	// Beyond the plan's list: the severity change itself, a postmortem kept
	// as typed, and a 403 for a viewer who posts anyway.
	it('patches a valid severity and keeps a postmortem as typed', async () => {
		const SEVERITY_ID = '9a1b2c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d';
		const sev = fakeEvent({ params, form: { severity_id: SEVERITY_ID } });
		await actions.severity(sev.event);
		expect(sev.calls()).toEqual([{ method: 'PATCH', path: base, search: '', body: JSON.stringify({ severity_id: SEVERITY_ID }) }]);
		const pm = fakeEvent({ params, form: { postmortem: '# Cause\n\n  indented' } });
		await actions.postmortem(pm.event);
		// Multipart encoding sends CRLF, which the engine normalises to LF.
		expect(pm.calls()[0].body).toBe(JSON.stringify({ postmortem: '# Cause\r\n\r\n  indented' }));
	});

	it("shows the engine's 403 as the form message", async () => {
		const { event } = fakeEvent({ params, form: { body: 'hi' }, status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.comment(event)).toMatchObject({ status: 403, data: { message: 'forbidden' } });
	});
});

function detail(overrides: Partial<IncidentDetail> = {}): IncidentDetail {
	return {
		id: INCIDENT_ID,
		project_id: PROJECT_ID,
		monitor_id: MONITOR_ID,
		heartbeat_id: null,
		log_alert_id: null,
		source: 'monitor',
		source_name: 'API',
		title: 'API is down',
		cause: 'connection refused',
		status: 'open',
		severity: 'high',
		severity_rank: 2,
		started_at: '2026-10-13T10:00:00Z',
		acknowledged_at: null,
		acknowledged_by_email: null,
		resolved_at: null,
		details: null,
		severity_id: null,
		metadata: {},
		postmortem: null,
		resolved_by_email: null,
		created_by_email: null,
		events: [
			{ id: 1, kind: 'opened', actor_id: null, actor_email: null, message: null, metadata: {}, created_at: '2026-10-13T10:00:00Z' },
			{ id: 2, kind: 'comment', actor_id: 'u', actor_email: 'ann@b.test', message: 'line one\nline two', metadata: {}, created_at: '2026-10-13T10:05:00Z' }
		],
		...overrides
	};
}

function renderPage(role: 'admin' | 'member' | 'viewer', incident: IncidentDetail = detail()) {
	const data = {
		user: { id: 'u', email: 'a@b.test', global_role: 'member' },
		project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
		role,
		incident,
		severities: [{ id: '9a1b2c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d', name: 'high', rank: 2, is_default: true }]
	};
	return render(Page, { props: { params: { id: PROJECT_ID, incidentId: INCIDENT_ID }, data, form: null } as never }).body;
}

describe('incident detail page', () => {
	it('shows no action, severity, comment or postmortem form to a viewer', () => {
		const body = renderPage('viewer');
		for (const action of ['acknowledge', 'resolve', 'reopen', 'severity', 'comment', 'postmortem']) {
			expect(body).not.toContain(`action="?/${action}"`);
		}
		expect(body).toContain('API is down');
		expect(body).toContain('Incident opened');
	});

	it('shows a member the actions that fit the status', () => {
		const open = renderPage('member');
		for (const action of ['acknowledge', 'resolve', 'severity', 'comment', 'postmortem']) expect(open).toContain(`action="?/${action}"`);
		expect(open).not.toContain('action="?/reopen"');

		const acked = renderPage('member', detail({ status: 'acknowledged', acknowledged_at: '2026-10-13T10:02:00Z', acknowledged_by_email: 'ann@b.test' }));
		expect(acked).not.toContain('action="?/acknowledge"');
		expect(acked).toContain('action="?/resolve"');
		expect(acked).toContain('Acknowledged by ann@b.test');

		const resolved = renderPage('admin', detail({ status: 'resolved', resolved_at: '2026-10-13T11:00:00Z', resolved_by_email: 'bob@b.test' }));
		expect(resolved).toContain('Resolved by bob@b.test at 2026-10-13 11:00:00 UTC');
		expect(resolved).toContain('action="?/reopen"');
		expect(resolved).not.toContain('action="?/resolve"');
		expect(resolved).not.toContain('action="?/acknowledge"');
	});

	it('links the source and shows the timeline in UTC', () => {
		const body = renderPage('viewer');
		expect(body).toContain(`href="/projects/${PROJECT_ID}/monitors/${MONITOR_ID}"`);
		expect(body).toContain('2026-10-13 10:05:00 UTC');
		expect(body).toContain('ann@b.test');
		expect(body).toContain('line one\nline two');
	});

	it('renders the postmortem through the markdown renderer only', () => {
		const body = renderPage('viewer', detail({ postmortem: '# Cause\n\n<script>alert(1)</script> **db**' }));
		expect(body).toContain('<h1>Cause</h1>');
		expect(body).toContain('&lt;script&gt;alert(1)&lt;/script&gt; <strong>db</strong>');
		expect(body).not.toContain('<script>alert(1)');
	});

	it('keeps hostile text, cause and job output as text', () => {
		const hostile = '<img src=x onerror=alert(1)>';
		const body = renderPage(
			'member',
			detail({
				title: hostile,
				cause: hostile,
				details: `${hostile}\nbackup failed`,
				events: [{ id: 1, kind: hostile, actor_id: null, actor_email: null, message: hostile, metadata: {}, created_at: '2026-10-13T10:00:00Z' }]
			})
		);
		expect(body).not.toContain('<img');
		expect(body).toContain('<details>');
		expect(body).toContain('Job output');
	});
});
