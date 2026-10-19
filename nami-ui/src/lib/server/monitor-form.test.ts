import { describe, expect, it } from 'vitest';
import { monitorPayloadFromForm, parseHeaderLines } from './monitor-form';

function form(fields: Record<string, string>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) data.set(key, value);
	return data;
}

/** The checkboxes the form renders checked by default. */
const defaults = { follow_redirects: 'on', verify_ssl: 'on' };

describe('monitorPayloadFromForm', () => {
	it('sends http fields and parsed numbers for an http monitor', () => {
		const result = monitorPayloadFromForm(
			form({
				...defaults,
				name: ' API ',
				check_type: 'http',
				url: ' https://example.com ',
				http_method: 'HEAD',
				expected_status_min: '200',
				expected_status_max: '299',
				interval_seconds: '30',
				timeout_seconds: '5',
				confirmation_threshold: '3',
				recovery_threshold: '2',
				host: 'ignored',
				keyword: 'ignored'
			})
		);
		expect(result).toEqual({
			ok: true,
			payload: {
				name: 'API',
				check_type: 'http',
				url: 'https://example.com',
				http_method: 'HEAD',
				follow_redirects: true,
				verify_ssl: true,
				expected_status_min: 200,
				expected_status_max: 299,
				interval_seconds: 30,
				timeout_seconds: 5,
				confirmation_threshold: 3,
				recovery_threshold: 2
			}
		});
	});

	it('sends host and port, never a url, for a tcp monitor', () => {
		const result = monitorPayloadFromForm(
			form({ name: 'DB', check_type: 'tcp', host: 'db.internal', port: '5432', url: 'https://stale.test' })
		);
		expect(result).toEqual({ ok: true, payload: { name: 'DB', check_type: 'tcp', host: 'db.internal', port: 5432 } });
	});

	it('keeps the keyword untrimmed and defaults the mode', () => {
		const result = monitorPayloadFromForm(
			form({ name: 'Home', check_type: 'keyword', url: 'https://example.com', keyword: ' Welcome ' })
		);
		expect(result.ok && result.payload.keyword).toBe(' Welcome ');
		expect(result.ok && result.payload.keyword_mode).toBe('contains');
	});

	it('omits blank numbers so the engine defaults apply', () => {
		const result = monitorPayloadFromForm(form({ name: 'API', check_type: 'http', url: 'https://x.test', interval_seconds: '' }));
		expect(result.ok && 'interval_seconds' in result.payload).toBe(false);
	});

	it('rejects non-whole numbers', () => {
		expect(monitorPayloadFromForm(form({ name: 'API', check_type: 'http', interval_seconds: '1.5' }))).toEqual({
			ok: false,
			message: 'interval_seconds must be a whole number'
		});
		expect(monitorPayloadFromForm(form({ name: 'DB', check_type: 'tcp', port: 'abc' }))).toEqual({
			ok: false,
			message: 'port must be a whole number'
		});
	});

	it('sends the request body only with POST, PUT or PATCH', () => {
		const post = monitorPayloadFromForm(
			form({ name: 'API', check_type: 'http', url: 'https://x.test', http_method: 'POST', request_body: '{"a":1}' })
		);
		expect(post.ok && post.payload.request_body).toBe('{"a":1}');
		const get = monitorPayloadFromForm(
			form({ name: 'API', check_type: 'http', url: 'https://x.test', http_method: 'GET', request_body: '{"a":1}' })
		);
		expect(get.ok && 'request_body' in get.payload).toBe(false);
	});

	it('on create sends filled-in secrets and skips empty ones', () => {
		const result = monitorPayloadFromForm(
			form({
				name: 'API',
				check_type: 'http',
				url: 'https://x.test',
				request_headers: 'X-Api-Key: abc:def\r\n\r\nX-Tenant:acme',
				auth_username: 'monitor',
				auth_password: 's3cret'
			})
		);
		expect(result.ok && result.payload.request_headers).toEqual([
			{ name: 'X-Api-Key', value: 'abc:def' },
			{ name: 'X-Tenant', value: 'acme' }
		]);
		expect(result.ok && result.payload.auth_username).toBe('monitor');
		expect(result.ok && result.payload.auth_password).toBe('s3cret');

		const empty = monitorPayloadFromForm(form({ name: 'API', check_type: 'http', url: 'https://x.test', auth_password: '' }));
		expect(empty.ok && ['request_headers', 'auth_username', 'auth_password'].some((k) => k in empty.payload)).toBe(false);
	});

	it('on edit keeps, replaces or clears each stored secret as chosen', () => {
		const base = { name: 'API', check_type: 'http', url: 'https://x.test', auth_username: 'monitor' };
		const keep = monitorPayloadFromForm(
			form({ ...base, headers_action: 'keep', password_action: 'keep', request_headers: 'X-Ignored: 1' }),
			'edit'
		);
		expect(keep.ok && 'request_headers' in keep.payload).toBe(false);
		expect(keep.ok && 'auth_password' in keep.payload).toBe(false);

		const replace = monitorPayloadFromForm(
			form({ ...base, headers_action: 'replace', request_headers: 'X-New: 2', password_action: 'replace', auth_password: 'n3w' }),
			'edit'
		);
		expect(replace.ok && replace.payload.request_headers).toEqual([{ name: 'X-New', value: '2' }]);
		expect(replace.ok && replace.payload.auth_password).toBe('n3w');

		const clear = monitorPayloadFromForm(form({ ...base, headers_action: 'clear', password_action: 'clear' }), 'edit');
		expect(clear.ok && clear.payload.request_headers).toBeNull();
		expect(clear.ok && clear.payload.auth_password).toBeNull();
	});

	it('on edit sends null for blank optional fields so they are cleared', () => {
		const http = monitorPayloadFromForm(
			form({ name: 'API', check_type: 'http', url: 'https://x.test', http_method: 'GET', auth_username: '', group_id: '' }),
			'edit'
		);
		expect(http.ok && http.payload.request_body).toBeNull();
		expect(http.ok && http.payload.auth_username).toBeNull();
		expect(http.ok && http.payload.group_id).toBeNull();
		const dns = monitorPayloadFromForm(
			form({ name: 'MX', check_type: 'dns', host: 'example.com', dns_record_type: 'MX', dns_nameserver: '', expected_value: '' }),
			'edit'
		);
		expect(dns.ok && dns.payload).toMatchObject({ dns_nameserver: null, expected_value: null });
	});

	it('refuses a replaced password left empty and a malformed header line', () => {
		expect(
			monitorPayloadFromForm(form({ name: 'A', check_type: 'http', password_action: 'replace', auth_password: '' }), 'edit')
		).toEqual({ ok: false, message: 'Enter the new password, or keep the current one' });
		expect(monitorPayloadFromForm(form({ name: 'A', check_type: 'http', request_headers: 'no colon here' }))).toEqual({
			ok: false,
			message: 'Header line 1 must look like "Name: value"'
		});
	});

	it('shapes every other check type', () => {
		const ok = (fields: Record<string, string>) => {
			const result = monitorPayloadFromForm(form({ name: 'M', ...fields }));
			if (!result.ok) throw new Error(result.message);
			return result.payload;
		};
		expect(ok({ check_type: 'ping', host: ' 10.0.0.1 ', port: '22' })).toEqual({ name: 'M', check_type: 'ping', host: '10.0.0.1' });
		expect(ok({ check_type: 'dns', host: 'example.com', dns_record_type: 'TXT', dns_nameserver: '1.1.1.1', expected_value: 'v=spf1' })).toEqual({
			name: 'M',
			check_type: 'dns',
			host: 'example.com',
			dns_record_type: 'TXT',
			dns_nameserver: '1.1.1.1',
			expected_value: 'v=spf1'
		});
		expect(ok({ check_type: 'ssl_expiry', host: 'example.com', port: '', expiry_threshold_days: '21' })).toEqual({
			name: 'M',
			check_type: 'ssl_expiry',
			host: 'example.com',
			expiry_threshold_days: 21
		});
		expect(ok({ check_type: 'domain_expiry', host: 'example.com', expiry_threshold_days: '45' })).toEqual({
			name: 'M',
			check_type: 'domain_expiry',
			host: 'example.com',
			expiry_threshold_days: 45
		});
		expect(ok({ check_type: 'imap', host: 'mail.example.com', port: '993', use_tls: 'on' })).toEqual({
			name: 'M',
			check_type: 'imap',
			host: 'mail.example.com',
			port: 993,
			use_tls: true
		});
		expect(ok({ check_type: 'smtp', host: 'mail.example.com' })).toEqual({
			name: 'M',
			check_type: 'smtp',
			host: 'mail.example.com',
			use_tls: false
		});
		expect(ok({ check_type: 'udp', host: 'game.example.com', port: '27015', udp_payload: 'PING\\r\\n', keyword: 'PONG' })).toEqual({
			name: 'M',
			check_type: 'udp',
			host: 'game.example.com',
			port: 27015,
			udp_payload: 'PING\r\n',
			keyword: 'PONG',
			keyword_mode: 'contains'
		});
	});

	it('sends the group, or nothing on create when ungrouped', () => {
		const grouped = monitorPayloadFromForm(form({ name: 'A', check_type: 'tcp', host: 'h', port: '1', group_id: 'g-1' }));
		expect(grouped.ok && grouped.payload.group_id).toBe('g-1');
		const ungrouped = monitorPayloadFromForm(form({ name: 'A', check_type: 'tcp', host: 'h', port: '1', group_id: '' }));
		expect(ungrouped.ok && 'group_id' in ungrouped.payload).toBe(false);
	});
});

describe('monitorPayloadFromForm after a type switch on edit', () => {
	const base = { name: 'M', host: 'example.com' };
	const ok = (fields: Record<string, string>) => {
		const result = monitorPayloadFromForm(form({ ...base, ...fields }), 'edit');
		if (!result.ok) throw new Error(result.message);
		return result.payload;
	};

	it('sends the expiry defaults for fields left empty when a ping becomes an SSL check', () => {
		expect(ok({ check_type: 'ssl_expiry', initial_check_type: 'ping' })).toMatchObject({
			interval_seconds: 86400,
			confirmation_threshold: 1,
			expiry_threshold_days: 14
		});
	});

	it('sends the plain defaults when an expiry check becomes a ping, and keeps typed values', () => {
		expect(ok({ check_type: 'ping', initial_check_type: 'domain_expiry' })).toMatchObject({
			interval_seconds: 60,
			confirmation_threshold: 2
		});
		expect(ok({ check_type: 'ping', initial_check_type: 'domain_expiry', interval_seconds: '45' })).toMatchObject({
			interval_seconds: 45
		});
	});

	it('resets only the threshold between the two expiry types', () => {
		const payload = ok({ check_type: 'domain_expiry', initial_check_type: 'ssl_expiry' });
		expect(payload).toMatchObject({ expiry_threshold_days: 30 });
		expect(payload).not.toHaveProperty('interval_seconds');
	});

	it('adds nothing without a switch, and nothing on create', () => {
		expect(ok({ check_type: 'ping', initial_check_type: 'ping' })).not.toHaveProperty('interval_seconds');
		const created = monitorPayloadFromForm(form({ ...base, check_type: 'ssl_expiry', initial_check_type: 'ping' }));
		expect(created.ok && 'interval_seconds' in created.payload).toBe(false);
	});
});

describe('parseHeaderLines', () => {
	it('splits at the first colon and trims around it', () => {
		expect(parseHeaderLines('Authorization: Bearer a:b\nX-Key: v')).toEqual({
			ok: true,
			headers: [
				{ name: 'Authorization', value: 'Bearer a:b' },
				{ name: 'X-Key', value: 'v' }
			]
		});
		expect(parseHeaderLines('  \n')).toEqual({ ok: true, headers: [] });
	});
	it('names the first malformed line', () => {
		expect(parseHeaderLines('X-Ok: 1\n: no name')).toEqual({ ok: false, message: 'Header line 2 must look like "Name: value"' });
	});
	// "Replace all" starts with one "Name: " line per stored header: one left
	// unfilled would replace a stored secret with nothing.
	it('refuses a line without a value', () => {
		for (const text of ['X-Api-Key:', 'X-Ok: 1\nX-Api-Key: ', 'X-Ok: 1\nX-Api-Key:\t']) {
			const line = text.split('\n').length;
			expect(parseHeaderLines(text)).toEqual({ ok: false, message: `Header line ${line} needs a value` });
		}
	});
});

describe('monitorPayloadFromForm keeps secrets on their origin', () => {
	// The hidden fields the edit form renders for a monitor with secrets.
	const stored = {
		name: 'API',
		check_type: 'http',
		auth_username: 'monitor',
		initial_url: 'https://api.test/health',
		initial_verify_ssl: 'on',
		stored_headers: 'on',
		stored_password: 'on',
		...defaults
	};
	const edit = (fields: Record<string, string>) => monitorPayloadFromForm(form({ ...stored, ...fields }), 'edit');

	it('refuses to keep stored secrets when the url moves to another origin', () => {
		const cases: Record<string, string>[] = [
			{ headers_action: 'keep', password_action: 'keep' },
			{ headers_action: 'replace', request_headers: 'X-Api-Key: new', password_action: 'keep' },
			{ headers_action: 'keep', password_action: 'replace', auth_password: 'new' }
		];
		for (const fields of cases) {
			expect(edit({ url: 'https://evil.test/steal', ...fields })).toEqual({
				ok: false,
				message: "Changing the URL's scheme, host or port means entering the header values and password again, or removing them"
			});
		}
		expect(edit({ url: 'http://api.test/health', headers_action: 'keep', password_action: 'clear' }).ok).toBe(false);
	});

	it('lets the url move once every stored secret is entered again or removed', () => {
		const moved = edit({
			url: 'https://other.test/h',
			headers_action: 'replace',
			request_headers: 'X-Api-Key: new',
			password_action: 'clear'
		});
		expect(moved.ok && moved.payload).toMatchObject({ request_headers: [{ name: 'X-Api-Key', value: 'new' }], auth_password: null });
	});

	it('keeps them for a path change, and for a monitor without secrets', () => {
		const path = edit({ url: 'https://api.test/v2?x=1', headers_action: 'keep', password_action: 'keep' });
		expect(path.ok).toBe(true);
		const bare = monitorPayloadFromForm(
			form({ ...stored, stored_headers: '', stored_password: '', url: 'https://other.test/', headers_action: 'keep', password_action: 'keep' }),
			'edit'
		);
		expect(bare.ok).toBe(true);
	});

	it('refuses to keep them when certificate checks are turned off', () => {
		const { verify_ssl: _, ...unchecked } = stored;
		const result = monitorPayloadFromForm(
			form({ ...unchecked, url: 'https://api.test/health', headers_action: 'keep', password_action: 'keep' }),
			'edit'
		);
		expect(result).toEqual({
			ok: false,
			message: 'Turning off certificate verification means entering the header values and password again, or removing them'
		});
	});
});

describe('monitorPayloadFromForm on edit, empty number fields', () => {
	it('resets the expiry threshold to the type default with null and keeps the others', () => {
		const result = monitorPayloadFromForm(
			form({ name: 'Cert', check_type: 'ssl_expiry', initial_check_type: 'ssl_expiry', host: 'example.com', interval_seconds: '' }),
			'edit'
		);
		expect(result.ok && result.payload.expiry_threshold_days).toBeNull();
		expect(result.ok && 'interval_seconds' in result.payload).toBe(false);
	});
});
