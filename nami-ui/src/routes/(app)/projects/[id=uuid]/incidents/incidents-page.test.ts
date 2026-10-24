// Renders the incidents page on the server (no browser): where each incident
// links to, which tab is current, and that text from a target stays text.
import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { HEARTBEAT_ID, MONITOR_ID, PROJECT_ID } from '#lib/server/core/fake-event';
import type { Incident } from '#lib/incidents/incidents';
import Page from './+page.svelte';

function incident(overrides: Partial<Incident> = {}): Incident {
	return {
		id: 'i1',
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

function renderPage(incidents: Incident[], status: 'open' | 'resolved' = 'resolved') {
	const data = {
		user: { id: 'u', email: 'a@b.test', global_role: 'member' },
		project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
		incidents,
		status
	};
	return render(Page, { props: { params: { id: PROJECT_ID }, data, form: null } as never });
}

describe('incidents page', () => {
	it('links a monitor incident to its monitor and a heartbeat incident to its heartbeat', () => {
		const { body } = renderPage([
			incident(),
			incident({ id: 'i2', monitor_id: null, heartbeat_id: HEARTBEAT_ID, title: 'Backup missed its heartbeat' })
		]);
		expect(body).toContain(`<a href="/projects/${PROJECT_ID}/monitors/${MONITOR_ID}" class="hover:text-primary">API is down</a>`);
		expect(body).toContain(
			`<a href="/projects/${PROJECT_ID}/heartbeats/${HEARTBEAT_ID}" class="hover:text-primary">Backup missed its heartbeat</a>`
		);
	});

	it('links a log alert incident to its alert', () => {
		const ALERT_ID = '3d0c5b1e-8f7a-4c2d-9e6b-1a2b3c4d5e6f';
		const { body } = renderPage([
			incident({ monitor_id: null, log_alert_id: ALERT_ID, title: 'Log alert Checkout errors triggered' })
		]);
		expect(body).toContain(
			`<a href="/projects/${PROJECT_ID}/logs/alerts/${ALERT_ID}" class="hover:text-primary">Log alert Checkout errors triggered</a>`
		);
	});

	it('links only a log alert id that is a UUID', () => {
		const { body } = renderPage([incident({ monitor_id: null, log_alert_id: '../../auth/me', title: 'Odd alert' })]);
		expect(body).toContain('Odd alert');
		expect(body).not.toContain('Odd alert</a>');
		expect(body).not.toContain('/logs/alerts/');
	});

	it('shows an incident without a source as plain text', () => {
		const { body } = renderPage([incident({ monitor_id: null, heartbeat_id: null, title: 'Orphan incident' })]);
		expect(body).toContain('Orphan incident');
		expect(body).not.toContain('Orphan incident</a>');
	});

	it('shows the cause, when it started and when it was resolved', () => {
		const { body } = renderPage([incident()]);
		expect(body).toContain('connection refused');
		expect(body).toContain('Started 2026-10-13 10:00:00 UTC');
		expect(body).toContain('resolved 2026-10-13 10:01:11 UTC');
		expect(body).toContain('Resolved · 1m 11s');
	});

	it('shows an open incident as ongoing, without a resolution time', () => {
		const { body } = renderPage([incident({ status: 'open', resolved_at: null })], 'open');
		expect(body).toContain('Ongoing · ');
		expect(body).not.toContain('· resolved');
	});

	// The open tab lists unresolved incidents: an acknowledged one is ongoing too.
	it('shows an acknowledged incident as ongoing', () => {
		const { body } = renderPage(
			[incident({ status: 'acknowledged', acknowledged_at: '2026-10-13T10:00:30Z', resolved_at: null })],
			'open'
		);
		expect(body).toContain('Ongoing · ');
		expect(body).not.toContain('Resolved · ');
		expect(body).toContain('bg-danger-soft');
	});

	it('marks the current tab', () => {
		const open = renderPage([], 'open').body;
		expect(open).toMatch(/<a href="\?status=open" aria-current="page"[^>]*>Open<\/a>/);
		expect(open).toMatch(/<a href="\?status=resolved"(?! aria-current)[^>]*>Resolved<\/a>/);

		const resolved = renderPage([], 'resolved').body;
		expect(resolved).toMatch(/<a href="\?status=resolved" aria-current="page"[^>]*>Resolved<\/a>/);
		expect(resolved).toMatch(/<a href="\?status=open"(?! aria-current)[^>]*>Open<\/a>/);
	});

	it('says so when a tab has no incidents', () => {
		expect(renderPage([], 'open').body).toContain('No open incidents — everything is up.');
		expect(renderPage([], 'resolved').body).toContain('No resolved incidents yet.');
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
