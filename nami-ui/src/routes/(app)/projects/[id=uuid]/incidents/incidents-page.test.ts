// Renders the incidents page on the server (no browser): where each incident
// links to, which tab is current, who sees the "New incident" form, and that
// text from a target stays text.
import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { HEARTBEAT_ID, MONITOR_ID, PROJECT_ID } from '#lib/server/core/fake-event';
import type { Incident, ListQuery } from '#lib/incidents/incidents';
import Page from './+page.svelte';

const INCIDENT_ID = '3f0c8a52-7d1e-4b6a-9c2f-5e8d7a6b4c31';

function incident(overrides: Partial<Incident> = {}): Incident {
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
		status: 'resolved',
		severity: 'high',
		severity_rank: 2,
		started_at: '2026-10-13T10:00:00Z',
		acknowledged_at: null,
		acknowledged_by_email: null,
		resolved_at: '2026-10-13T10:01:11Z',
		details: null,
		...overrides
	};
}

function renderPage(
	incidents: Incident[],
	query: Partial<ListQuery> = {},
	extra: { role?: 'admin' | 'member' | 'viewer'; nextCursor?: string | null } = {}
) {
	const data = {
		user: { id: 'u', email: 'a@b.test', global_role: 'member' },
		project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
		role: extra.role ?? 'viewer',
		incidents,
		nextCursor: extra.nextCursor ?? null,
		query: { tab: 'unresolved', source: null, cursor: null, ...query },
		severities: [{ id: '9a1b2c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d', name: 'critical', rank: 1, is_default: false }],
		policies: [{ id: '7e6d5c4b-3a29-4180-9f7e-6d5c4b3a2918', name: 'Night shift', is_default: false }]
	};
	return render(Page, { props: { params: { id: PROJECT_ID }, data, form: null } as never });
}

describe('incidents page', () => {
	it('links each title to its incident and the source to its monitor or heartbeat', () => {
		const SECOND = '5b7a9c1d-2e3f-4a5b-8c6d-7e8f9a0b1c2d';
		const { body } = renderPage([
			incident(),
			incident({ id: SECOND, monitor_id: null, heartbeat_id: HEARTBEAT_ID, source: 'heartbeat', source_name: 'Backup', title: 'Backup missed its heartbeat' })
		]);
		expect(body).toContain(`href="/projects/${PROJECT_ID}/incidents/${INCIDENT_ID}"`);
		expect(body).toContain(`href="/projects/${PROJECT_ID}/incidents/${SECOND}"`);
		expect(body).toContain(`<a href="/projects/${PROJECT_ID}/monitors/${MONITOR_ID}" class="link">API</a>`);
		expect(body).toContain(`<a href="/projects/${PROJECT_ID}/heartbeats/${HEARTBEAT_ID}" class="link">Backup</a>`);
	});

	it('links a log alert incident to its alert', () => {
		const ALERT_ID = '3d0c5b1e-8f7a-4c2d-9e6b-1a2b3c4d5e6f';
		const { body } = renderPage([
			incident({ monitor_id: null, log_alert_id: ALERT_ID, source: 'log_alert', source_name: 'Checkout errors', title: 'Log alert Checkout errors triggered' })
		]);
		expect(body).toContain(`<a href="/projects/${PROJECT_ID}/logs/alerts/${ALERT_ID}" class="link">Checkout errors</a>`);
	});

	it('links only a log alert id that is a UUID', () => {
		const { body } = renderPage([incident({ monitor_id: null, log_alert_id: '../../auth/me', source: 'log_alert', source_name: 'Odd alert' })]);
		expect(body).toContain('Odd alert');
		expect(body).not.toContain('Odd alert</a>');
		expect(body).not.toContain('/logs/alerts/');
	});

	it('shows an incident without a source link as plain text', () => {
		const { body } = renderPage([incident({ monitor_id: null, source: 'manual', source_name: null, title: 'Orphan incident' })]);
		expect(body).toContain('Orphan incident');
		expect(body).toContain('Manual');
		expect(body).not.toContain('Manual</a>');
	});

	it('shows severity, cause, start, duration, acknowledger and status', () => {
		const { body } = renderPage([incident({ acknowledged_by_email: 'ann@b.test' })]);
		expect(body).toContain('high');
		expect(body).toContain('connection refused');
		expect(body).toContain('2026-10-13 10:00:00 UTC');
		expect(body).toContain('1 min');
		expect(body).toContain('ann@b.test');
		expect(body).toContain('badge-neutral">Resolved');
	});

	// Acknowledged incidents are unresolved: they keep a warning badge.
	it('marks open and acknowledged incidents', () => {
		expect(renderPage([incident({ status: 'open', resolved_at: null })]).body).toContain('badge-danger">Open');
		expect(renderPage([incident({ status: 'acknowledged', resolved_at: null })]).body).toContain('badge-warning">Acknowledged');
	});

	it('marks the current tab and keeps the source in the tab links', () => {
		const body = renderPage([], { tab: 'resolved', source: 'heartbeat' }).body;
		expect(body).toMatch(/<a href="\?tab=resolved&amp;source=heartbeat" aria-current="page"[^>]*>Resolved<\/a>/);
		expect(body).toMatch(/<a href="\?tab=unresolved&amp;source=heartbeat"(?! aria-current)[^>]*>Unresolved<\/a>/);
		expect(body).toMatch(/<option value="heartbeat" selected[^>]*>Heartbeat</);
	});

	it('carries the cursor in the "Load more" link, and offers the newest page from a later one', () => {
		const first = renderPage([incident()], { tab: 'all' }, { nextCursor: 'abc_-' }).body;
		expect(first).toContain('href="?tab=all&amp;cursor=abc_-"');
		expect(first).not.toContain('Newest');
		const later = renderPage([incident()], { tab: 'all', cursor: 'abc_-' }).body;
		expect(later).toContain('Newest');
		expect(later).not.toContain('Load more');
	});

	it('says so when a tab has no incidents', () => {
		expect(renderPage([]).body).toContain('No unresolved incidents — everything is up.');
		expect(renderPage([], { tab: 'resolved' }).body).toContain('No resolved incidents yet.');
	});

	it('shows the "New incident" form to members and admins, never to viewers', () => {
		expect(renderPage([]).body).not.toContain('action="?/create"');
		for (const role of ['member', 'admin'] as const) {
			const body = renderPage([], {}, { role }).body;
			expect(body).toContain('action="?/create"');
			expect(body).toContain('Night shift');
			expect(body).toContain('critical');
		}
	});

	// A title or cause can carry text from the target (a heartbeat's name, an
	// error message): it must reach the page as text.
	it('escapes a hostile title and cause', () => {
		const hostile = '<img src=x onerror="window.__xss=1"> & <script>alert(1)</script>';
		const { body } = renderPage([incident({ title: hostile, cause: hostile })]);
		expect(body).not.toContain('<img');
		expect(body).not.toContain('<script');
		expect(body.match(/&lt;img src=x onerror="window\.__xss=1"> &amp; &lt;script>alert\(1\)&lt;\/script>/g)).toHaveLength(2);
	});

	it('sets the page title', () => {
		expect(renderPage([]).head).toContain('Incidents · Demo');
	});

	it('shows job output as escaped text in a details block, and none without it', () => {
		const { body } = renderPage([incident({ details: '<script>alert(1)</script>\nbackup failed' })]);
		expect(body).toContain('<details>');
		expect(body).toContain('Job output');
		expect(body).toContain('&lt;script>alert(1)&lt;/script>');
		expect(body).not.toContain('<script>alert(1)</script>');
		expect(renderPage([incident({ details: null })]).body).not.toContain('<details');
	});
});
