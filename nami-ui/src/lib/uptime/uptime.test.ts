import { execFileSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import {
	escapePayload,
	formatClockUtc,
	formatDuration,
	formatMs,
	formatSeconds,
	formatUptime,
	formatUtc,
	exitCodeCommand,
	failUrl,
	groupMonitors,
	isRedacted,
	proxyMustBeReentered,
	secretsMustBeReentered,
	SECRETS_ORIGIN_MESSAGE,
	SECRETS_TLS_MESSAGE,
	monitorTarget,
	niceTicks,
	pingCommand,
	pingUrl,
	shellQuote,
	statusSince,
	timeAgo,
	unescapePayload,
	type Incident
} from './uptime';

describe('formatUptime', () => {
	it('shows two decimals and a dash for unknown', () => {
		expect(formatUptime(null)).toBe('—');
		expect(formatUptime(undefined)).toBe('—');
		expect(formatUptime(1)).toBe('100.00%');
		expect(formatUptime(0.75)).toBe('75.00%');
		expect(formatUptime(0)).toBe('0.00%');
	});

	// Review Focus: uptime that is not exactly 100% never displays as 100.00%.
	it('truncates instead of rounding, so any downtime stays visible', () => {
		expect(formatUptime(0.99999)).toBe('99.99%');
		expect(formatUptime(86_399 / 86_400)).toBe('99.99%');
		expect(formatUptime(0.12349)).toBe('12.34%');
	});

	it('is not thrown off by floating-point error', () => {
		expect(formatUptime(0.57)).toBe('57.00%'); // 0.57 * 10000 = 5699.999…
		expect(formatUptime(0.29)).toBe('29.00%');
	});
});

describe('formatMs', () => {
	it('rounds to whole milliseconds', () => {
		expect(formatMs(199.6)).toBe('200 ms');
		expect(formatMs(null)).toBe('—');
	});
});

describe('timeAgo', () => {
	const now = Date.parse('2026-10-13T12:00:00Z');
	it('picks the largest whole unit', () => {
		expect(timeAgo(null, now)).toBe('never');
		expect(timeAgo('2026-10-13T11:59:48Z', now)).toBe('12s ago');
		expect(timeAgo('2026-10-13T11:55:00Z', now)).toBe('5m ago');
		expect(timeAgo('2026-10-13T09:00:00Z', now)).toBe('3h ago');
		expect(timeAgo('2026-10-11T12:00:00Z', now)).toBe('2d ago');
	});
	it('never goes negative for timestamps slightly in the future', () => {
		expect(timeAgo('2026-10-13T12:00:02Z', now)).toBe('0s ago');
	});
});

describe('formatDuration', () => {
	const now = Date.parse('2026-10-13T12:00:00Z');
	it('uses two units at most', () => {
		expect(formatDuration('2026-10-13T11:59:15Z', '2026-10-13T12:00:00Z', now)).toBe('45s');
		expect(formatDuration('2026-10-13T11:57:30Z', '2026-10-13T12:00:00Z', now)).toBe('2m 30s');
		expect(formatDuration('2026-10-13T09:55:00Z', '2026-10-13T12:00:00Z', now)).toBe('2h 5m');
		expect(formatDuration('2026-10-10T08:00:00Z', '2026-10-13T12:00:00Z', now)).toBe('3d 4h');
	});
	it('measures an ongoing incident up to now', () => {
		expect(formatDuration('2026-10-13T11:00:00Z', null, now)).toBe('1h 0m');
	});
});

describe('UTC formatting', () => {
	it('normalizes engine timestamps of any precision', () => {
		expect(formatUtc('2026-10-13T10:00:30.123456Z')).toBe('2026-10-13 10:00:30 UTC');
		expect(formatUtc('2026-10-13T10:00:30.5Z')).toBe('2026-10-13 10:00:30 UTC');
		expect(formatClockUtc('2026-10-13T09:30:00Z')).toBe('09:30');
	});
});

describe('monitorTarget', () => {
	it('shows the url or host:port', () => {
		expect(monitorTarget({ check_type: 'http', url: 'https://x.test', host: null, port: null })).toBe('https://x.test');
		expect(monitorTarget({ check_type: 'tcp', url: null, host: 'db', port: 5432 })).toBe('db:5432');
	});
});

describe('niceTicks', () => {
	it('ends on a round value at or above the max', () => {
		expect(niceTicks(183)).toEqual([0, 50, 100, 150, 200]);
		expect(niceTicks(1234)).toEqual([0, 500, 1000, 1500]);
		expect(niceTicks(40)).toEqual([0, 10, 20, 30, 40]);
		expect(niceTicks(7)).toEqual([0, 2, 4, 6, 8]);
	});
	it('has a default scale when there is no data', () => {
		expect(niceTicks(0)).toEqual([0, 50, 100, 150, 200]);
	});
	// Response times are whole milliseconds, so the axis never needs a step below 1.
	it('uses whole-millisecond steps when the maximum is below one tick', () => {
		expect(niceTicks(0.5)).toEqual([0, 1, 2, 3, 4]);
		expect(niceTicks(0.0001)).toEqual([0, 1, 2, 3, 4]);
		expect(niceTicks(3)).toEqual([0, 1, 2, 3, 4]);
	});
	it('never labels a tick with float noise', () => {
		for (const max of [0.1, 0.3, 0.7, 1.5, 3.3, 9.9, 99.9, 123.456, 1999.9]) {
			for (const tick of niceTicks(max)) expect(Number.isInteger(tick), `max ${max}: ${tick}`).toBe(true);
		}
	});
	it('still ends on a round value for large maxima', () => {
		expect(niceTicks(60_000)).toEqual([0, 20_000, 40_000, 60_000]);
		expect(niceTicks(123_456_789)).toEqual([0, 50_000_000, 100_000_000, 150_000_000]);
	});
});

describe('statusSince', () => {
	const created = '2026-10-01T00:00:00Z';
	const incident = (status: Incident['status'], started: string, resolved: string | null): Incident => ({
		id: started,
		project_id: 'p',
		monitor_id: 'm',
		heartbeat_id: null,
		log_alert_id: null,
		source: 'monitor',
		source_name: 'API',
		title: 'API is down',
		cause: null,
		status,
		severity: 'high',
		severity_rank: 2,
		started_at: started,
		acknowledged_at: null,
		acknowledged_by_email: null,
		resolved_at: resolved,
		details: null
	});
	const history = [
		incident('resolved', '2026-10-05T00:00:00Z', '2026-10-05T01:00:00Z'),
		incident('resolved', '2026-10-09T00:00:00Z', '2026-10-09T00:30:00Z')
	];

	it('is the open incident start while down', () => {
		const open = incident('open', '2026-10-12T00:00:00Z', null);
		expect(statusSince({ current_status: 'down', created_at: created }, [open, ...history])).toBe(open.started_at);
	});
	it('is the acknowledged incident start while down', () => {
		const acked = incident('acknowledged', '2026-10-12T00:00:00Z', null);
		expect(statusSince({ current_status: 'down', created_at: created }, [acked, ...history])).toBe(acked.started_at);
	});
	it('is the latest recovery while up', () => {
		expect(statusSince({ current_status: 'up', created_at: created }, history)).toBe('2026-10-09T00:30:00Z');
	});
	it('falls back to creation', () => {
		expect(statusSince({ current_status: 'up', created_at: created }, [])).toBe(created);
		expect(statusSince({ current_status: 'pending', created_at: created }, history)).toBe(created);
	});
});

describe('pingUrl', () => {
	it('joins the engine URL and token without a double slash', () => {
		expect(pingUrl('https://engine.example.com/', 'abc')).toBe('https://engine.example.com/api/v1/heartbeat/abc');
		expect(pingUrl('http://localhost:8080', 'abc')).toBe('http://localhost:8080/api/v1/heartbeat/abc');
	});

	it('keeps a path prefix of the engine URL', () => {
		expect(pingUrl('https://example.com/engine//', 'abc')).toBe('https://example.com/engine/api/v1/heartbeat/abc');
	});

	// Tokens are hex, but the URL is built from engine data: whatever it holds
	// stays inside its own path segment.
	it('keeps a token inside its own path segment', () => {
		expect(pingUrl('http://localhost:8080', 'a/../b?c#d')).toBe(
			'http://localhost:8080/api/v1/heartbeat/a%2F..%2Fb%3Fc%23d'
		);
	});
});

describe('formatSeconds', () => {
	it('shows at most two units', () => {
		expect(formatSeconds(86_400)).toBe('1d');
		expect(formatSeconds(5_400)).toBe('1h 30m');
		expect(formatSeconds(90)).toBe('1m 30s');
		expect(formatSeconds(90_061)).toBe('1d 1h');
		expect(formatSeconds(0)).toBe('0s');
	});

	it('covers the whole range a heartbeat can have (30 s to 31 days)', () => {
		expect(formatSeconds(30)).toBe('30s');
		expect(formatSeconds(59)).toBe('59s');
		expect(formatSeconds(61)).toBe('1m 1s');
		expect(formatSeconds(3_599)).toBe('59m 59s');
		expect(formatSeconds(3_661)).toBe('1h 1m');
		expect(formatSeconds(2_678_400)).toBe('31d');
	});

	it('never shows a negative, fractional or unknown amount', () => {
		expect(formatSeconds(-5)).toBe('0s');
		expect(formatSeconds(90.9)).toBe('1m 30s');
		expect(formatSeconds(Number.NaN)).toBe('0s');
	});
});

// The shell tests ask /bin/sh, so they need a POSIX system (this repo's machines
// and containers are).
const posix = process.platform !== 'win32';

describe('shellQuote', () => {
	it('wraps the value in single quotes', () => {
		expect(shellQuote('https://engine.example.com/api/v1/heartbeat/abc')).toBe(
			"'https://engine.example.com/api/v1/heartbeat/abc'"
		);
		expect(shellQuote('')).toBe("''");
	});

	it('closes, escapes and reopens the quote around a single quote', () => {
		expect(shellQuote("it's")).toBe("'it'\\''s'");
		expect(shellQuote("'")).toBe("''\\'''");
	});

	// The only way to know a quoting function is right is to ask a shell: every
	// value, whatever it holds, comes back as one word, unchanged, and nothing
	// in it runs.
	it.skipIf(!posix)('is read back unchanged, as one word, by a real shell', () => {
		const values = [
			'plain',
			'https://engine.example.com/a b/api/v1/heartbeat/abc',
			"it's",
			"a'b'c",
			"''",
			'x\'; echo pwned; \'',
			'$(echo pwned)',
			'`echo pwned`',
			'${HOME}',
			'a;b&c|d>e<f',
			'*?[a-z]~',
			'"double"',
			'back\\slash',
			'100%',
			'#not-a-comment',
			'multi\nline',
			'tab\there',
			'ünï€ode'
		];
		for (const value of values) {
			const out = execFileSync('/bin/sh', ['-c', `printf %s ${shellQuote(value)}`], { encoding: 'utf8' });
			expect(out, JSON.stringify(value)).toBe(value);
			// Two words, not one: the quoting must not have merged or split anything.
			const count = execFileSync('/bin/sh', ['-c', `set -- ${shellQuote(value)} ${shellQuote(value)}; printf %s $#`], { encoding: 'utf8' });
			expect(count, JSON.stringify(value)).toBe('2');
		}
	});
});

describe('pingCommand', () => {
	it('is the curl command of the crontab example, with the URL quoted', () => {
		expect(pingCommand('http://localhost:8080/api/v1/heartbeat/abc')).toBe(
			"curl -fsS -m 10 --retry 3 'http://localhost:8080/api/v1/heartbeat/abc'"
		);
	});

	it.skipIf(!posix)('keeps a URL with shell characters in it as one argument', () => {
		const url = "https://example.com/it's;$(echo pwned)&x=1/api/v1/heartbeat/abc";
		const echoed = execFileSync('/bin/sh', ['-c', pingCommand(url).replace(/^curl /, 'printf "%s\\n" ')], { encoding: 'utf8' });
		// curl's options are separate words, the URL is the last one, intact.
		expect(echoed.trimEnd().split('\n')).toEqual(['-fsS', '-m', '10', '--retry', '3', url]);
	});
});

describe('monitorTarget for every check type', () => {
	const base = { url: null, host: 'example.com', port: 443, dns_record_type: null, dns_nameserver: null };
	it('picks the field each type checks', () => {
		expect(monitorTarget({ ...base, check_type: 'ping' })).toBe('example.com');
		expect(monitorTarget({ ...base, check_type: 'domain_expiry' })).toBe('example.com');
		expect(monitorTarget({ ...base, check_type: 'ssl_expiry' })).toBe('example.com:443');
		expect(monitorTarget({ ...base, check_type: 'udp', port: 27015 })).toBe('example.com:27015');
		expect(monitorTarget({ ...base, check_type: 'smtp', port: 25 })).toBe('example.com:25');
	});
	it('shows the record type and nameserver of a DNS monitor', () => {
		expect(monitorTarget({ ...base, check_type: 'dns', dns_record_type: 'MX' })).toBe('MX example.com');
		expect(monitorTarget({ ...base, check_type: 'dns', dns_record_type: 'A', dns_nameserver: '1.1.1.1:53' })).toBe(
			'A example.com @1.1.1.1:53'
		);
	});
});

describe('groupMonitors', () => {
	const group = (id: string, name: string, position: number) => ({
		id,
		project_id: 'p',
		name,
		position,
		created_at: '2026-10-13T00:00:00Z'
	});
	const monitor = (id: string, group_id: string | null) => ({ id, group_id });

	it('keeps the engine order of groups and puts ungrouped monitors last', () => {
		const groups = [group('g1', 'APIs', 0), group('g2', 'Websites', 1)];
		const sections = groupMonitors(
			[monitor('a', 'g2'), monitor('b', null), monitor('c', 'g1'), monitor('d', 'g2')],
			groups
		);
		expect(sections.map((s) => [s.group?.name ?? null, s.monitors.map((m) => m.id)])).toEqual([
			['APIs', ['c']],
			['Websites', ['a', 'd']],
			[null, ['b']]
		]);
	});

	it('drops empty groups and treats an unknown group as ungrouped', () => {
		const sections = groupMonitors([monitor('a', 'gone')], [group('g1', 'Empty', 0)]);
		expect(sections).toEqual([{ group: null, monitors: [monitor('a', 'gone')] }]);
	});

	it('is a single ungrouped section when the project has no groups', () => {
		expect(groupMonitors([monitor('a', null)], [])).toEqual([{ group: null, monitors: [monitor('a', null)] }]);
		expect(groupMonitors([], [])).toEqual([]);
	});
});

describe('UDP payload escapes', () => {
	it('round-trips control characters through one-line text', () => {
		expect(unescapePayload('PING\\r\\n\\tx\\\\y')).toBe('PING\r\n\tx\\y');
		expect(escapePayload('PING\r\n\tx\\y')).toBe('PING\\r\\n\\tx\\\\y');
		expect(unescapePayload(escapePayload('a\\nb\n'))).toBe('a\\nb\n');
		expect(unescapePayload('no escapes \\q')).toBe('no escapes \\q');
	});
});

describe('isRedacted', () => {
	it('follows the engine flag, never the text', () => {
		expect(isRedacted({ redacted: true })).toBe(true);
		expect(isRedacted({ redacted: false })).toBe(false);
	});
});

describe('secretsMustBeReentered', () => {
	const edit = {
		storedUrl: 'https://api.test/health',
		url: 'https://api.test/health',
		storedVerifySsl: true,
		verifySsl: true
	};
	it('lets a path or query change on the same origin keep the secrets', () => {
		expect(secretsMustBeReentered(edit)).toBeNull();
		expect(secretsMustBeReentered({ ...edit, url: 'https://api.test/v2/health?deep=1' })).toBeNull();
		expect(secretsMustBeReentered({ ...edit, url: ' https://API.test:443/x ' })).toBeNull();
	});
	it('asks for them again when the scheme, host or port changes', () => {
		for (const url of [
			'http://api.test/health',
			'https://evil.test/health',
			'https://api.test:8443/health',
			'not a url'
		]) {
			expect(secretsMustBeReentered({ ...edit, url })).toBe(SECRETS_ORIGIN_MESSAGE);
		}
	});
	it('asks for them again when certificate checks are turned off over https', () => {
		expect(secretsMustBeReentered({ ...edit, verifySsl: false })).toBe(SECRETS_TLS_MESSAGE);
		expect(
			secretsMustBeReentered({ ...edit, storedUrl: 'HTTPS://api.test/h', url: 'HTTPS://api.test/h', verifySsl: false })
		).toBe(SECRETS_TLS_MESSAGE);
		expect(secretsMustBeReentered({ ...edit, storedVerifySsl: false, verifySsl: false })).toBeNull();
	});
});

describe('heartbeat failure examples', () => {
	it('builds the fail URL and an exit-code command that reports output', () => {
		const url = 'https://engine.example.com/api/v1/heartbeat/' + 'a'.repeat(64);
		expect(failUrl(url)).toBe(url + '/fail');
		expect(exitCodeCommand(url)).toBe(
			`out=$(your-job 2>&1); code=$?; printf '%s' "$out" | tail -c 10240 | curl -fsS -m 10 --retry 3 --data-binary @- '${url}'/$code`
		);
		// Never `--data-binary "$out"`: output starting with @ would make curl upload a local file.
		expect(exitCodeCommand(url)).not.toContain('--data-binary "$out"');
		// Only the end of the output (where errors are) is sent, so a chatty
		// job never trips the 1 MiB cap, and a busy engine is retried.
		expect(exitCodeCommand(url)).toContain('| tail -c 10240 |');
		expect(exitCodeCommand(url)).toContain('--retry 3');
	});

	it('groups heartbeats like monitors', () => {
		const group = { id: 'g1', project_id: 'p', name: 'Backups', position: 0, created_at: '2026-10-13T00:00:00Z' };
		const sections = groupMonitors([{ group_id: 'g1' }, { group_id: null }], [group]);
		expect(sections.map((s) => [s.group?.name ?? null, s.monitors.length])).toEqual([
			['Backups', 1],
			[null, 1]
		]);
	});
});

describe('proxyMustBeReentered', () => {
	const moved = 'Changing the proxy requires entering the passwords and header values again, or clearing them';
	const check = (storedProxyUrl: string | null, proxyUrl: string) => proxyMustBeReentered({ storedProxyUrl, proxyUrl });

	it('compares origins, not spellings', () => {
		expect(check('http://proxy:3128', 'http://PROXY:3128/other')).toBeNull();
		expect(check('http://P', 'http://p:80')).toBeNull();
		expect(check('http://p:80', 'http://P')).toBeNull();
	});
	it('asks again for another scheme, host or port', () => {
		expect(check('http://p:3128', 'https://p:3128')).toBe(moved);
		expect(check('http://p:3128', 'http://q:3128')).toBe(moved);
		expect(check('http://p:3128', 'http://p:3129')).toBe(moved);
	});
	it('counts an unparsable url as a change, stored or new', () => {
		expect(check('http://p:3128', 'not a url')).toBe(moved);
		expect(check('not a url', 'http://p:3128')).toBe(moved);
	});
	it('counts adding a proxy as a change and removing one as exempt', () => {
		expect(check(null, 'http://p:3128')).toBe(moved);
		expect(check('http://p:3128', '')).toBeNull();
		expect(check('http://p:3128', '   ')).toBeNull();
		expect(check(null, '')).toBeNull();
	});
});
