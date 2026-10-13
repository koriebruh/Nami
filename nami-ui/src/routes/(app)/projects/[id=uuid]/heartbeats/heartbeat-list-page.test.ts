// Renders the list page on the server (no browser), to pin down what a viewer
// and a member are shown. The live behaviour of the page (copying, refreshing,
// the submit guard) is covered by the walkthrough, which needs a browser.
import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { HEARTBEAT_ID, PROJECT_ID } from '#lib/server/fake-event';
import type { Heartbeat } from '#lib/uptime';
import Page from './+page.svelte';

const TOKEN = 'f'.repeat(64);
const ENGINE = 'http://engine.test';

function heartbeat(overrides: Partial<Heartbeat> = {}): Heartbeat {
	return {
		id: HEARTBEAT_ID,
		project_id: PROJECT_ID,
		name: 'Nightly backup',
		token: TOKEN,
		period_seconds: 86_400,
		grace_seconds: 60,
		paused: false,
		current_status: 'up',
		last_ping_at: '2026-10-13T10:00:00Z',
		created_at: '2026-10-12T10:00:00Z',
		...overrides
	};
}

function renderPage(heartbeats: Heartbeat[], form: unknown = null) {
	const data = {
		user: { id: 'u', email: 'a@b.test', global_role: 'member' },
		project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
		heartbeats,
		engineUrl: ENGINE
	};
	return render(Page, { props: { params: { id: PROJECT_ID }, data, form } as never });
}

describe('heartbeat list page', () => {
	it('shows a member the ping URL of every heartbeat, with a copy button', () => {
		const { body } = renderPage([heartbeat()]);
		expect(body).toContain(`${ENGINE}/api/v1/heartbeat/${TOKEN}`);
		expect(body).toContain('>Copy<');
		expect(body).not.toContain('visible to project members');
	});

	// The engine gives a viewer `token: null`. Nothing on the page may look like
	// a ping URL then, in any attribute or text.
	it('shows a viewer no ping URL, only a note', () => {
		const { body } = renderPage([heartbeat({ token: null }), heartbeat({ id: 'other', name: 'Other', token: null })]);
		expect(body).not.toContain('/api/v1/heartbeat/');
		expect(body).not.toContain('>Copy<');
		expect(body.match(/Ping URL is visible to project members\./g)).toHaveLength(2);
		// The rest of the row is still there.
		expect(body).toContain('Nightly backup');
		expect(body).toContain('every 1d');
	});

	it('decides per heartbeat', () => {
		const { body } = renderPage([heartbeat({ name: 'Mine' }), heartbeat({ id: 'other', name: 'Theirs', token: null })]);
		expect(body.match(/\/api\/v1\/heartbeat\//g)).toHaveLength(1);
		expect(body.match(/Ping URL is visible to project members\./g)).toHaveLength(1);
	});

	it('links each heartbeat to its own page and names the ping URL for screen readers', () => {
		const { body } = renderPage([heartbeat()]);
		expect(body).toContain(`href="/projects/${PROJECT_ID}/heartbeats/${HEARTBEAT_ID}"`);
		expect(body).toContain('aria-label="Ping URL of Nightly backup"');
	});

	it('shows the status, the period and the grace', () => {
		const { body } = renderPage([heartbeat({ current_status: 'down', period_seconds: 5_400, grace_seconds: 0 })]);
		expect(body).toContain('Down');
		expect(body).toContain('every 1h 30m');
		expect(body).toContain('grace 0s');
	});

	it('says so when there are no heartbeats', () => {
		expect(renderPage([]).body).toContain('No heartbeats yet');
	});

	// A name is text from a user: it must reach the page as text, in the
	// markup, the attributes and the title.
	it('escapes a hostile name', () => {
		const name = '<img src=x onerror="window.__xss=1"> & "q"';
		const { body } = renderPage([heartbeat({ name })]);
		// No tag opens anywhere: the "<" of the name is escaped in the text, and
		// the quotes are escaped in the attribute that holds the name too.
		expect(body).not.toContain('<img');
		expect(body).toContain('&lt;img src=x onerror="window.__xss=1"> &amp; "q"</a>');
		expect(body).toContain('aria-label="Ping URL of &lt;img src=x onerror=&quot;window.__xss=1&quot;> &amp; &quot;q&quot;"');
	});

	it('offers the create form with the engine limits on its inputs', () => {
		const { body } = renderPage([]);
		expect(body).toContain('name="period_seconds"');
		expect(body).toMatch(/name="period_seconds"[^>]*min="30"[^>]*max="2678400"|min="30"[^>]*max="2678400"[^>]*name="period_seconds"/);
		expect(body).toMatch(/name="grace_seconds"[^>]*min="0"[^>]*max="2678400"|min="0"[^>]*max="2678400"[^>]*name="grace_seconds"/);
		expect(body).toMatch(/name="name"[^>]*maxlength="200"|maxlength="200"[^>]*name="name"/);
	});

	it('announces a form error as an alert, escaped', () => {
		const { body } = renderPage([], { message: 'period_seconds must be at least 30 <b>' });
		expect(body).toContain('role="alert"');
		expect(body).toContain('period_seconds must be at least 30 &lt;b>');
		expect(body).not.toContain('<b>');
		expect(renderPage([]).body).not.toContain('role="alert"');
	});

	it('sets the page title', () => {
		expect(renderPage([]).head).toContain('Heartbeats · Demo');
	});
});
