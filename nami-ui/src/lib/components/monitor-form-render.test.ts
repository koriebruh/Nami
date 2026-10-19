// Server-side render of the monitor form: what a member and a viewer see of
// a stored monitor, and that no secret value ever reaches the markup.
import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import type { Monitor } from '#lib/uptime/uptime';
import MonitorForm from './MonitorForm.svelte';

function monitor(overrides: Partial<Monitor> = {}): Monitor {
	return {
		id: 'm',
		project_id: 'p',
		name: 'API',
		check_type: 'http',
		url: 'https://example.com/health',
		host: null,
		port: null,
		http_method: 'POST',
		keyword: null,
		keyword_mode: 'contains',
		expected_status_min: 200,
		expected_status_max: 399,
		interval_seconds: 60,
		timeout_seconds: 10,
		confirmation_threshold: 2,
		paused: false,
		current_status: 'up',
		consecutive_failures: 0,
		last_checked_at: null,
		next_check_at: '2026-10-13T00:00:00Z',
		created_at: '2026-10-13T00:00:00Z',
		request_body: '{"a":1}',
		request_headers: [{ name: 'X-Api-Key', value_set: true }],
		auth_username: 'monitor',
		has_auth_password: true,
		follow_redirects: true,
		verify_ssl: true,
		recovery_threshold: 1,
		consecutive_successes: 0,
		dns_record_type: null,
		dns_nameserver: null,
		expected_value: null,
		expiry_threshold_days: null,
		use_tls: false,
		udp_payload: null,
		group_id: null,
		in_maintenance: false,
		redacted: false,
		ip_version: 'any',
		remember_cookies: false,
		proxy_url: null,
		proxy_username: null,
		has_proxy_password: false,
		...overrides
	};
}

const props = { submitLabel: 'Save changes', cancelHref: '/x' };

describe('monitor form render', () => {
	it('shows stored header names and the password state, never a value', () => {
		const { body } = render(MonitorForm, { props: { ...props, monitor: monitor() } });
		expect(body).toContain('Stored: X-Api-Key');
		expect(body).toContain('Keep (set)');
		expect(body).toContain('>Save changes<');
		expect(body).not.toContain('read-only');
	});

	it('shows a viewer the masked monitor read-only, without a save button', () => {
		const { body } = render(MonitorForm, {
			props: {
				...props,
				monitor: monitor({ request_body: '***', auth_username: '***', url: 'https://example.com/h?k=***', redacted: true })
			}
		});
		expect(body).toContain('read-only');
		expect(body).not.toContain('>Save changes<');
		expect(body).toMatch(/<fieldset[^>]*disabled/);
	});

	// The engine's flag decides, not the text: a member's own URL can hold `***`.
	it('lets a member edit a monitor whose url contains a literal ***', () => {
		const { body } = render(MonitorForm, {
			props: { ...props, monitor: monitor({ url: 'https://example.com/***/health', request_body: '***' }) }
		});
		expect(body).not.toContain('read-only');
		expect(body).toContain('>Save changes<');
		expect(body).not.toMatch(/<fieldset[^>]*disabled/);
	});

	it('tells the form action which secrets are stored and where they may go', () => {
		const { body } = render(MonitorForm, { props: { ...props, monitor: monitor() } });
		expect(body).toMatch(/name="initial_url"[^>]*value="https:\/\/example.com\/health"/);
		expect(body).toMatch(/name="stored_headers"/);
		expect(body).toMatch(/name="stored_password"/);
		expect(body).toMatch(/name="initial_verify_ssl"/);
		const bare = render(MonitorForm, {
			props: { ...props, monitor: monitor({ request_headers: [], has_auth_password: false, verify_ssl: false }) }
		}).body;
		expect(bare).not.toMatch(/name="stored_(headers|password)"|name="initial_verify_ssl"/);
		// Same origin as stored: "Keep" stays available, nothing is forced.
		expect(body).not.toMatch(/name="headers_action" value="keep"[^>]*disabled/);
	});

	it('says on edit that empty number fields keep the current value', () => {
		expect(render(MonitorForm, { props: { ...props, monitor: monitor() } }).body).toContain('Empty fields keep the current value.');
		expect(render(MonitorForm, { props: { ...props, monitor: null } }).body).toContain('Empty fields use the default shown.');
	});

	it('hints at the final URL for secret-bearing monitors and caps field sizes', () => {
		const { body } = render(MonitorForm, { props: { ...props, monitor: null } });
		expect(body).toContain('point the monitor at the final URL');
		expect(body).toContain('maxlength="2048"');
		const post = render(MonitorForm, { props: { ...props, monitor: monitor() } }).body;
		expect(post).toContain('maxlength="65536"');
	});

	it('starts an expiry monitor from the expiry defaults', () => {
		const { body } = render(MonitorForm, {
			props: { ...props, monitor: monitor({ check_type: 'ssl_expiry', url: null, host: 'example.com', port: 443, interval_seconds: 86400, confirmation_threshold: 1, expiry_threshold_days: 21 }) }
		});
		expect(body).toContain('min="3600"');
		expect(body).toContain('placeholder="86400"');
		expect(body).toContain('placeholder="1"');
	});

	describe('advanced options', () => {
		it('offers the ip version for tcp and not for dns, and the proxy only for http', () => {
			const tcp = render(MonitorForm, { props: { ...props, monitor: monitor({ check_type: 'tcp', url: null, host: 'db', port: 5432 }) } }).body;
			expect(tcp).toContain('>Advanced<');
			expect(tcp).toContain('name="ip_version"');
			expect(tcp).not.toContain('name="proxy_url"');
			expect(tcp).not.toContain('name="remember_cookies"');
			const dns = render(MonitorForm, { props: { ...props, monitor: monitor({ check_type: 'dns', url: null, host: 'x.test' }) } }).body;
			expect(dns).not.toContain('name="ip_version"');
			expect(dns).not.toContain('>Advanced<');
			const http = render(MonitorForm, { props: { ...props, monitor: null } }).body;
			expect(http).toContain('name="proxy_url"');
			expect(http).toContain('name="remember_cookies"');
			expect(http).toContain('autocomplete="new-password"');
			expect(http).toContain('The proxy resolves the target, so IP version must be Any.');
		});

		it('shows keep, replace and remove for a stored proxy password and never its value', () => {
			const { body } = render(MonitorForm, {
				props: { ...props, monitor: monitor({ proxy_url: 'http://proxy:3128', proxy_username: 'nami', has_proxy_password: true }) }
			});
			expect(body).toMatch(/name="proxy_password_action" value="keep"/);
			expect(body).toMatch(/name="proxy_password_action" value="replace"/);
			expect(body).toMatch(/name="proxy_password_action" value="clear"/);
			expect(body).toContain('Keep (set)');
			expect(body).toMatch(/name="stored_proxy_password"/);
			expect(body).toMatch(/name="initial_proxy_url"[^>]*value="http:\/\/proxy:3128"/);
			expect(body).not.toMatch(/name="proxy_password"[^>]*value=/);
			expect(body).not.toMatch(/name="proxy_password_action" value="keep"[^>]*disabled/);
		});
	});
});
