// Renders the detail page on the server (no browser), to pin down what a
// viewer and a member are shown and where each message goes. The behaviour of
// the page in a browser (copying, confirm dialogs, the submit guard, typed
// values surviving a refresh) is covered by the walkthrough.
import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { selectRange } from '#lib/uptime/availability';
import { HEARTBEAT_ID, PROJECT_ID } from '#lib/server/core/fake-event';
import type { Heartbeat, HeartbeatPing, MonitorGroup } from '#lib/uptime/uptime';
import Page from './+page.svelte';

const TOKEN = 'c'.repeat(64);
const ENGINE = 'http://engine.test';
const PING = `${ENGINE}/api/v1/heartbeat/${TOKEN}`;

function heartbeat(overrides: Partial<Heartbeat> = {}): Heartbeat {
	return {
		id: HEARTBEAT_ID,
		project_id: PROJECT_ID,
		name: 'Nightly backup',
		token: TOKEN,
		period_seconds: 86_400,
		grace_seconds: 600,
		paused: false,
		current_status: 'up',
		last_ping_at: '2026-10-13T10:00:00Z',
		created_at: '2026-10-12T10:00:00Z',
		in_maintenance: false,
		group_id: null,
		...overrides
	};
}

function renderPage(hb: Heartbeat, form: unknown = null, pings: HeartbeatPing[] = [], groups: MonitorGroup[] = []) {
	const data = {
		user: { id: 'u', email: 'a@b.test', global_role: 'member' },
		project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
		heartbeat: hb,
		pings,
		groups,
		engineUrl: ENGINE,
		availability: { range: selectRange(new URLSearchParams(), new Date()), data: null, unavailable: false }
	};
	return render(Page, { props: { params: { id: PROJECT_ID, heartbeatId: HEARTBEAT_ID }, data, form } as never });
}

/** Where a piece of text sits relative to the three cards (header, Ping URL, Settings). */
function card(body: string, text: string): 'header' | 'ping' | 'settings' | 'missing' {
	const at = body.indexOf(text);
	if (at === -1) return 'missing';
	if (at < body.indexOf('>Ping URL<')) return 'header';
	return at < body.indexOf('>Settings<') ? 'ping' : 'settings';
}

describe('heartbeat detail page: ping URL', () => {
	it('shows a member the ping URL and a crontab line that uses it', () => {
		const { body } = renderPage(heartbeat());
		expect(body).toContain(`<code class="block flex-1`);
		expect(body.split(PING).length - 1).toBe(4); // the URL, the crontab line, the fail URL and the exit-code command
		// The command quotes the URL; the URL block above it is the plain URL.
		expect(body).toContain(`0 3 * * * /usr/local/bin/backup.sh &amp;&amp; curl -fsS -m 10 --retry 3 '${PING}'`);
		expect(body).toContain(`select-all">${PING}</code>`);
		expect(body).toContain('>Rotate token<');
		expect(body).not.toContain('visible to project members');
	});

	// The engine gives a viewer `token: null`: no URL, no command, no way to rotate.
	it('shows a viewer no ping URL, no command and no rotate button, only a note', () => {
		const { body } = renderPage(heartbeat({ token: null }));
		expect(body).not.toContain('/api/v1/heartbeat/');
		expect(body).not.toContain('curl');
		expect(body).not.toContain('crontab');
		expect(body).not.toContain('Rotate token');
		expect(body).not.toContain('>Copy<');
		expect(body.match(/Ping URL is visible to project members\./g)).toHaveLength(1);
		// The rest of the page is there.
		expect(body).toContain('Nightly backup');
		expect(body).toContain('Last ping:');
		expect(body).toContain('Expected every 1d, with 10m grace.');
	});

	it('does not show a viewer the rotate confirmation either, whatever the form says', () => {
		const { body } = renderPage(heartbeat({ token: null }), { confirmRotate: true });
		expect(body).not.toContain('Yes, rotate');
		expect(body).not.toContain('Rotate the ping token');
	});
});

describe('heartbeat detail page: header', () => {
	it('offers Pause for a running heartbeat and Resume for a paused one', () => {
		const running = renderPage(heartbeat()).body;
		expect(running).toContain('action="?/pause"');
		expect(running).toContain('>Pause<');
		expect(running).not.toContain('>Resume<');

		const paused = renderPage(heartbeat({ paused: true })).body;
		expect(paused).toContain('action="?/resume"');
		expect(paused).toContain('>Resume<');
		expect(paused).toContain('Paused');
		expect(paused).not.toContain('>Pause<');
	});

	it('shows when the last ping was, or that there was none', () => {
		expect(renderPage(heartbeat()).body).toContain('2026-10-13 10:00:00 UTC');
		expect(renderPage(heartbeat({ last_ping_at: null, current_status: 'pending' })).body).toContain('never');
	});

	it('sets the page title', () => {
		expect(renderPage(heartbeat()).head).toContain('Nightly backup · Heartbeats');
	});

	it('escapes a hostile name, in the heading, the title and the confirmation', () => {
		const name = '<img src=x onerror="window.__xss=1"> & "q"';
		const { body, head } = renderPage(heartbeat({ name }), { confirmDelete: true });
		expect(body).not.toContain('<img');
		expect(head).not.toContain('<img');
		expect(body).toContain('&lt;img src=x onerror="window.__xss=1"> &amp; "q"');
		expect(head).toContain('&lt;img src=x onerror="window.__xss=1"> &amp; "q" · Heartbeats');
		// The name is also in the settings input: an attribute, so quotes are escaped too.
		expect(body).toContain('value="&lt;img src=x onerror=&quot;window.__xss=1&quot;> &amp; &quot;q&quot;"');
	});
});

describe('heartbeat detail page: settings', () => {
	it('fills the form with the saved values and limits it like the engine does', () => {
		const { body } = renderPage(heartbeat({ name: 'Backup', period_seconds: 3600, grace_seconds: 120 }));
		expect(body).toMatch(/id="hb-name"[^>]*maxlength="200"[^>]*value="Backup"/);
		expect(body).toMatch(/id="hb-period"[^>]*min="30"[^>]*max="2678400"[^>]*value="3600"/);
		expect(body).toMatch(/id="hb-grace"[^>]*min="0"[^>]*max="2678400"[^>]*value="120"/);
		expect(body).toContain('action="?/update"');
	});
});

describe('heartbeat detail page: what an action leaves in `form`', () => {
	it('asks for the second step of a delete without JavaScript', () => {
		const { body } = renderPage(heartbeat(), { confirmDelete: true });
		expect(body).toContain('Delete heartbeat "Nightly backup"? Its incidents are deleted too.');
		expect(body).toContain('>Yes, delete<');
		expect(body).toMatch(/<input type="hidden" name="confirm" value="yes"\/>/);
		expect(renderPage(heartbeat()).body).not.toContain('Yes, delete');
	});

	it('asks for the second step of a token rotation without JavaScript, and says the old URL stops working', () => {
		const { body } = renderPage(heartbeat(), { confirmRotate: true });
		expect(body).toContain('Rotate the ping token of "Nightly backup"?');
		expect(body).toContain('The old ping URL stops working immediately');
		expect(body).toContain('>Yes, rotate<');
		expect(renderPage(heartbeat()).body).not.toContain('Yes, rotate');
	});

	it('says that a token was rotated', () => {
		expect(renderPage(heartbeat(), { rotated: true }).body).toContain('Token rotated.');
		expect(renderPage(heartbeat()).body).not.toContain('Token rotated.');
	});

	it('says that settings were saved, and nothing after a pause or resume', () => {
		expect(renderPage(heartbeat(), { saved: true }).body).toContain('Saved.');
		expect(renderPage(heartbeat(), { success: true }).body).not.toContain('Saved.');
		expect(renderPage(heartbeat()).body).not.toContain('Saved.');
	});

	// Every action's failure arrives in the same `form`: each message goes in the
	// card of the control that was used, as an alert.
	it('puts an error beside the control that failed', () => {
		const message = 'forbidden';
		const at = (action: string) => card(renderPage(heartbeat(), { action, message }).body, `role="alert" class="mt-4 alert alert-danger">${message}`);
		const atRotate = card(renderPage(heartbeat(), { action: 'rotate', message }).body, `role="alert" class="alert alert-danger">${message}`);
		expect(at('pause')).toBe('header');
		expect(at('resume')).toBe('header');
		expect(at('delete')).toBe('header');
		expect(at('update')).toBe('settings');
		expect(atRotate).toBe('ping');
	});

	it('shows an error once, and escaped', () => {
		const { body } = renderPage(heartbeat(), { action: 'update', message: 'bad <script>alert(1)</script>' });
		expect(body.match(/role="alert"/g)).toHaveLength(1);
		expect(body).toContain('bad &lt;script>alert(1)&lt;/script>');
		expect(body).not.toContain('<script>alert(1)');
	});

	it('shows no alert when there is no error', () => {
		expect(renderPage(heartbeat()).body).not.toContain('role="alert"');
		expect(renderPage(heartbeat(), { saved: true }).body).not.toContain('role="alert"');
	});
});

describe('heartbeat detail page: maintenance', () => {
	it('shows the badge while a window covers the heartbeat', () => {
		expect(renderPage(heartbeat({ in_maintenance: true })).body).toContain('Maintenance');
		expect(renderPage(heartbeat()).body).not.toContain('Maintenance');
	});
});

describe('heartbeat detail page: failure examples', () => {
	it('shows /fail and the exit-code command to members only', () => {
		const member = renderPage(heartbeat()).body;
		expect(member).toContain(`${PING}/fail`);
		expect(member).toContain('| tail -c 10240 | curl -fsS -m 10 --retry 3 --data-binary @-');
		const viewer = renderPage(heartbeat({ token: null })).body;
		expect(viewer).not.toContain('/fail');
		expect(viewer).not.toContain('--data-binary');
	});
});

describe('heartbeat detail page: recent pings', () => {
	const ping = (overrides: Partial<HeartbeatPing> = {}): HeartbeatPing => ({
		heartbeat_id: HEARTBEAT_ID,
		project_id: PROJECT_ID,
		received_at: '2026-10-13T10:00:00Z',
		kind: 'fail',
		exit_code: 2,
		output: '<script>alert(1)</script>\nbackup failed',
		...overrides
	});

	it('shows output as text, never as markup', () => {
		const { body } = renderPage(heartbeat(), null, [ping()]);
		expect(body).toContain('&lt;script>alert(1)&lt;/script>');
		expect(body).not.toContain('<script>alert(1)</script>');
		expect(body).toContain('exit code 2');
		expect(body).toContain('2026-10-13 10:00:00 UTC');
	});

	it('has no output block for a ping without output, and says so when there are none', () => {
		const { body } = renderPage(heartbeat(), null, [ping({ kind: 'success', exit_code: null, output: '' })]);
		expect(body).toContain('Success');
		expect(body).not.toContain('>Output<');
		expect(renderPage(heartbeat()).body).toContain('No pings received yet.');
	});
});

describe('heartbeat detail page: group', () => {
	const groups: MonitorGroup[] = [
		{ id: 'g1', project_id: PROJECT_ID, name: 'Backups', position: 0, created_at: '2026-10-13T00:00:00Z' },
		{ id: 'g2', project_id: PROJECT_ID, name: 'Reports', position: 1, created_at: '2026-10-13T00:00:00Z' }
	];

	it('offers this project\'s groups with the saved one selected', () => {
		const { body } = renderPage(heartbeat({ group_id: 'g2' }), null, [], groups);
		expect(body).toContain('name="group_id"');
		expect(body).toContain('>No group<');
		expect(body).toMatch(/<option value="g2" selected[^>]*>Reports</);
		expect(body).not.toMatch(/<option value="g1" selected/);
	});

	it('keeps the saved group when the groups did not load', () => {
		const { body } = renderPage(heartbeat({ group_id: 'g2' }));
		expect(body).toMatch(/<input type="hidden" name="group_id" value="g2"/);
	});
});
