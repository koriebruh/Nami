// Shapes of nami-engine's uptime JSON, plus pure display helpers shared by
// the monitor and incident pages. All times render in UTC so server- and
// client-rendered markup agree (and match the alert emails).

import type { Incident } from '#lib/incidents/incidents';

// Existing imports of the incident list type keep working from here.
export type { Incident };

export type Status = 'pending' | 'up' | 'down';
export type CheckType =
	| 'http'
	| 'keyword'
	| 'tcp'
	| 'ping'
	| 'dns'
	| 'ssl_expiry'
	| 'domain_expiry'
	| 'smtp'
	| 'pop3'
	| 'imap'
	| 'udp';
export type HttpMethod = 'GET' | 'HEAD' | 'POST' | 'PUT' | 'PATCH' | 'DELETE' | 'OPTIONS';
export type DnsRecordType = 'A' | 'AAAA' | 'CNAME' | 'MX' | 'TXT' | 'NS';

export const CHECK_TYPE_LABELS: Record<CheckType, string> = {
	http: 'HTTP',
	keyword: 'Keyword',
	tcp: 'TCP',
	ping: 'Ping',
	dns: 'DNS',
	ssl_expiry: 'SSL expiry',
	domain_expiry: 'Domain expiry',
	smtp: 'SMTP',
	pop3: 'POP3',
	imap: 'IMAP',
	udp: 'UDP'
};

export const HTTP_METHODS: HttpMethod[] = ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS'];
export const BODY_METHODS: HttpMethod[] = ['POST', 'PUT', 'PATCH'];
export const DNS_RECORD_TYPES: DnsRecordType[] = ['A', 'AAAA', 'CNAME', 'MX', 'TXT', 'NS'];

export type IpVersion = 'any' | 'ipv4' | 'ipv6';
export const IP_VERSIONS: { value: IpVersion; label: string }[] = [
	{ value: 'any', label: 'Any' },
	{ value: 'ipv4', label: 'IPv4 only' },
	{ value: 'ipv6', label: 'IPv6 only' }
];
/** Check types that resolve a target themselves and so honour an IP version. */
export const IP_VERSION_TYPES: CheckType[] = ['http', 'keyword', 'tcp', 'ping', 'ssl_expiry'];

/** A custom request header as the engine returns it: the value is write-only. */
export type RequestHeaderView = { name: string; value_set: boolean };

export type Monitor = {
	id: string;
	project_id: string;
	name: string;
	check_type: CheckType;
	url: string | null;
	host: string | null;
	port: number | null;
	http_method: HttpMethod;
	keyword: string | null;
	keyword_mode: 'contains' | 'not_contains';
	expected_status_min: number;
	expected_status_max: number;
	interval_seconds: number;
	timeout_seconds: number;
	confirmation_threshold: number;
	paused: boolean;
	current_status: Status;
	consecutive_failures: number;
	last_checked_at: string | null;
	next_check_at: string;
	created_at: string;
	request_body: string | null;
	request_headers: RequestHeaderView[];
	auth_username: string | null;
	has_auth_password: boolean;
	follow_redirects: boolean;
	verify_ssl: boolean;
	recovery_threshold: number;
	consecutive_successes: number;
	dns_record_type: DnsRecordType | null;
	dns_nameserver: string | null;
	expected_value: string | null;
	expiry_threshold_days: number | null;
	use_tls: boolean;
	udp_payload: string | null;
	group_id: string | null;
	in_maintenance: boolean;
	/** True when fields were masked (`***`) for the reader, a viewer. */
	redacted: boolean;
	ip_version: IpVersion;
	remember_cookies: boolean;
	proxy_url: string | null;
	proxy_username: string | null;
	/** The proxy password is write-only: only whether one is stored. */
	has_proxy_password: boolean;
};

export type MonitorGroup = {
	id: string;
	project_id: string;
	name: string;
	position: number;
	created_at: string;
};

export type ImportRow = {
	line: number;
	status: 'created' | 'skipped' | 'error';
	name?: string;
	target?: string;
	monitor_id?: string;
	error?: string;
	warning?: string;
};

export type ImportReport = {
	dry_run: boolean;
	created: number;
	skipped: number;
	errors: number;
	results: ImportRow[];
};

export type MonitorSummary = Monitor & { uptime_24h: number | null };

export type UptimeCheck = {
	monitor_id: string;
	project_id: string;
	checked_at: string;
	status: 'up' | 'down';
	status_code: number | null;
	response_time_ms: number | null;
	error: string | null;
};

export type SeriesBucket = {
	bucket_start: string;
	avg_response_ms: number | null;
	up_ratio: number | null;
};

export type MonitorStats = {
	uptime_24h: number | null;
	uptime_7d: number | null;
	uptime_30d: number | null;
	avg_response_ms_24h: number | null;
	series: SeriesBucket[];
};

export type Heartbeat = {
	id: string;
	project_id: string;
	name: string;
	/**
	 * The ping secret. `null` for a viewer: the engine only gives it to members
	 * of the project (and global admins), because whoever holds it can ping.
	 */
	token: string | null;
	period_seconds: number;
	grace_seconds: number;
	paused: boolean;
	current_status: Status;
	last_ping_at: string | null;
	created_at: string;
	in_maintenance: boolean;
	group_id: string | null;
};

/** One ping a heartbeat received; `output` is plain text, never markup. */
export type HeartbeatPing = {
	heartbeat_id: string;
	project_id: string;
	received_at: string;
	kind: 'success' | 'fail';
	exit_code: number | null;
	output: string;
};

/** The URL a cron job pings: straight to nami-engine, not through this BFF. */
export function pingUrl(engineUrl: string, token: string): string {
	return `${engineUrl.replace(/\/+$/, '')}/api/v1/heartbeat/${encodeURIComponent(token)}`;
}

/**
 * `value` as one shell word: in single quotes, so that nothing in it (a space,
 * `&`, `;`, `$(...)`, a backtick) is read by the shell. A single quote inside
 * is closed, escaped and reopened: it's -> 'it'\''s'.
 */
export function shellQuote(value: string): string {
	return `'${value.replace(/'/g, `'\\''`)}'`;
}

/**
 * The command a cron job runs to ping: quiet unless it fails, 10 s to answer,
 * 3 retries. The URL is quoted, because it is built from configuration
 * (PUBLIC_ENGINE_URL) and the command is pasted into a shell.
 */
export function pingCommand(url: string): string {
	return `curl -fsS -m 10 --retry 3 ${shellQuote(url)}`;
}

/** The URL that reports the job as failed right away. */
export function failUrl(url: string): string {
	return `${url}/fail`;
}

/**
 * A command that runs the job, then reports its exit code and output. The
 * output goes in on stdin (`@-`), never as `--data-binary "$out"`: output that
 * starts with `@` would make curl upload a local file of that name. Only its
 * last 10 KiB are sent (`tail -c`: errors come last, and the engine keeps at
 * most 10 KiB), so the size is bounded, and curl retries like the plain ping
 * command, so a failure report is not lost to a busy moment.
 */
export function exitCodeCommand(url: string): string {
	return `out=$(your-job 2>&1); code=$?; printf '%s' "$out" | tail -c 10240 | curl -fsS -m 10 --retry 3 --data-binary @- ${shellQuote(url)}/$code`;
}

/** Compact period: 86400 -> "1d", 5400 -> "1h 30m", 90 -> "1m 30s". */
export function formatSeconds(total: number): string {
	const parts: string[] = [];
	let rest = Math.max(0, Math.floor(total));
	for (const [unit, size] of [
		['d', 86_400],
		['h', 3_600],
		['m', 60],
		['s', 1]
	] as const) {
		const amount = Math.floor(rest / size);
		rest -= amount * size;
		if (amount > 0) parts.push(`${amount}${unit}`);
	}
	return parts.length > 0 ? parts.slice(0, 2).join(' ') : '0s';
}

/**
 * Uptime ratio (0..1) as a percentage with two decimals, truncated rather
 * than rounded so downtime never disappears: 0.99999 shows "99.99%", never
 * "100.00%". The epsilon only absorbs floating-point error
 * (0.57 * 10000 = 5699.999…); real ratios are k/n with n < 100,000 checks
 * per window, so it cannot lift a lower value into the next hundredth.
 */
export function formatUptime(ratio: number | null | undefined): string {
	if (ratio === null || ratio === undefined || !Number.isFinite(ratio)) return '—';
	const hundredths = Math.floor(ratio * 10_000 + 1e-7);
	return `${(hundredths / 100).toFixed(2)}%`;
}

export function formatMs(ms: number | null | undefined): string {
	if (ms === null || ms === undefined || !Number.isFinite(ms)) return '—';
	return `${Math.round(ms)} ms`;
}

/** "12s ago", "5m ago", "3h ago", "2d ago"; "never" for null. */
export function timeAgo(iso: string | null, now: number): string {
	if (!iso) return 'never';
	const seconds = Math.max(0, Math.round((now - Date.parse(iso)) / 1000));
	if (seconds < 60) return `${seconds}s ago`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m ago`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h ago`;
	return `${Math.floor(hours / 24)}d ago`;
}

/** Elapsed time from `fromIso` to `toIso` (or to `now` while ongoing). */
export function formatDuration(fromIso: string, toIso: string | null, now: number): string {
	const end = toIso ? Date.parse(toIso) : now;
	let seconds = Math.max(0, Math.round((end - Date.parse(fromIso)) / 1000));
	const days = Math.floor(seconds / 86_400);
	seconds -= days * 86_400;
	const hours = Math.floor(seconds / 3_600);
	seconds -= hours * 3_600;
	const minutes = Math.floor(seconds / 60);
	seconds -= minutes * 60;
	if (days > 0) return `${days}d ${hours}h`;
	if (hours > 0) return `${hours}h ${minutes}m`;
	if (minutes > 0) return `${minutes}m ${seconds}s`;
	return `${seconds}s`;
}

/** "2026-10-13 14:30:05 UTC". */
export function formatUtc(iso: string): string {
	return `${new Date(iso).toISOString().slice(0, 19).replace('T', ' ')} UTC`;
}

/** "14:30" in UTC, for chart axes. */
export function formatClockUtc(iso: string): string {
	return new Date(iso).toISOString().slice(11, 16);
}

/**
 * What the monitor checks: the URL for http/keyword, the host for ping and
 * domain expiry, "TYPE host @nameserver" for DNS, host:port otherwise.
 */
export function monitorTarget(
	m: Pick<Monitor, 'check_type' | 'url' | 'host' | 'port'> &
		Partial<Pick<Monitor, 'dns_record_type' | 'dns_nameserver'>>
): string {
	switch (m.check_type) {
		case 'http':
		case 'keyword':
			return m.url ?? '';
		case 'ping':
		case 'domain_expiry':
			return m.host ?? '';
		case 'dns':
			return `${m.dns_record_type ?? 'A'} ${m.host ?? ''}${m.dns_nameserver ? ` @${m.dns_nameserver}` : ''}`;
		default:
			return `${m.host ?? ''}:${m.port ?? ''}`;
	}
}

const UNESCAPES: Record<string, string> = { n: '\n', r: '\r', t: '\t', '\\': '\\' };
const ESCAPES: Record<string, string> = { '\n': '\\n', '\r': '\\r', '\t': '\\t', '\\': '\\\\' };

/** UDP payloads are typed on one line: `\n`, `\r`, `\t` and `\\` are escapes. */
export function unescapePayload(text: string): string {
	return text.replace(/\\([nrt\\])/g, (_, c: string) => UNESCAPES[c]);
}

/** The inverse of {@link unescapePayload}, for showing a stored payload. */
export function escapePayload(text: string): string {
	return text.replace(/[\\\n\r\t]/g, (c) => ESCAPES[c]);
}

/**
 * Whether the engine masked parts of this monitor for its reader (a viewer:
 * URL secrets, request body, username and udp payload read `***`). A monitor
 * like that cannot be edited from what it shows: saving would write the mask
 * over the real values. Decided by the engine's flag, never by the text: a
 * member's URL can contain a literal `***`.
 */
export function isRedacted(m: Pick<Monitor, 'redacted'>): boolean {
	return m.redacted === true;
}

function schemeOf(url: string): string | null {
	try {
		return new URL(url).protocol;
	} catch {
		return null;
	}
}

function originOf(url: string | null): string | null {
	try {
		return url ? new URL(url).origin : null;
	} catch {
		return null;
	}
}

export const SECRETS_ORIGIN_MESSAGE =
	"Changing the URL's scheme, host or port means entering the header values and password again, or removing them";
export const SECRETS_TLS_MESSAGE =
	'Turning off certificate verification means entering the header values and password again, or removing them';

/**
 * Why the stored write-only secrets (header values, basic-auth password)
 * cannot be kept by an edit, or null when they can: the engine refuses to
 * send them to another origin (scheme, host, port) or over https without
 * certificate verification unless they are entered again or removed.
 */
export function secretsMustBeReentered(edit: {
	storedUrl: string | null;
	url: string;
	storedVerifySsl: boolean;
	verifySsl: boolean;
}): string | null {
	if (originOf(edit.storedUrl) === null || originOf(edit.storedUrl) !== originOf(edit.url.trim())) {
		return SECRETS_ORIGIN_MESSAGE;
	}
	if (edit.storedVerifySsl && !edit.verifySsl && schemeOf(edit.url.trim()) === 'https:') {
		return SECRETS_TLS_MESSAGE;
	}
	return null;
}

export const PROXY_MESSAGE =
	'Changing the proxy requires entering the passwords and header values again, or clearing them';

/**
 * Why the stored write-only secrets (header values, both passwords) cannot be
 * kept by an edit that points the proxy somewhere else, or null when they
 * can. Like the engine: a new proxy whose origin differs from the stored one
 * (or a proxy added where there was none) counts; removing the proxy does not.
 */
export function proxyMustBeReentered(edit: { storedProxyUrl: string | null; proxyUrl: string }): string | null {
	const next = edit.proxyUrl.trim();
	if (next === '') return null;
	const before = originOf(edit.storedProxyUrl);
	return before !== null && before === originOf(next) ? null : PROXY_MESSAGE;
}

export type MonitorSection<M> = { group: MonitorGroup | null; monitors: M[] };

/**
 * Monitors bucketed by group for the list page: groups in the engine's
 * order (position, then name), then the ungrouped ones (`group: null`).
 * Empty sections are dropped. A monitor whose group is unknown (e.g. just
 * deleted) counts as ungrouped.
 */
export function groupMonitors<M extends { group_id: string | null }>(
	monitors: M[],
	groups: MonitorGroup[]
): MonitorSection<M>[] {
	const known = new Set(groups.map((group) => group.id));
	const sections: MonitorSection<M>[] = groups.map((group) => ({
		group,
		monitors: monitors.filter((monitor) => monitor.group_id === group.id)
	}));
	sections.push({
		group: null,
		monitors: monitors.filter((monitor) => !monitor.group_id || !known.has(monitor.group_id))
	});
	return sections.filter((section) => section.monitors.length > 0);
}

/**
 * Clean y-axis ticks from 0 to a round value >= max (steps of 1/2/5 x 10^n).
 * Response times are whole milliseconds, so a step is never below 1: a maximum
 * under `count` gets the axis 0..count instead of fractional ticks.
 */
export function niceTicks(max: number, count = 4): number[] {
	if (!(max > 0)) return [0, 50, 100, 150, 200];
	max = Math.max(max, count);
	const rough = max / count;
	const magnitude = 10 ** Math.floor(Math.log10(rough));
	const step = [1, 2, 5, 10].map((m) => m * magnitude).find((s) => s >= rough) ?? 10 * magnitude;
	const top = Math.ceil(max / step) * step;
	// toPrecision drops any float noise a multiplication leaves behind.
	return Array.from({ length: Math.round(top / step) + 1 }, (_, i) => Number((i * step).toPrecision(12)));
}

/**
 * When the monitor entered its current status. The engine stores no
 * state-change timestamp, so derive it from incidents: down since the
 * unresolved (open or acknowledged) incident started; up since the latest recovery (or creation, if it never
 * went down); pending since creation.
 */
export function statusSince(
	monitor: Pick<Monitor, 'current_status' | 'created_at'>,
	incidents: Incident[]
): string {
	if (monitor.current_status === 'down') {
		return incidents.find((i) => i.status !== 'resolved')?.started_at ?? monitor.created_at;
	}
	if (monitor.current_status === 'up') {
		let latest: string | null = null;
		for (const incident of incidents) {
			if (incident.resolved_at && (!latest || Date.parse(incident.resolved_at) > Date.parse(latest))) {
				latest = incident.resolved_at;
			}
		}
		return latest ?? monitor.created_at;
	}
	return monitor.created_at;
}
