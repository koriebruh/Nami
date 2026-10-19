// Shapes of nami-engine's log JSON, plus pure helpers for the log pages.
// All times render in UTC (as on the uptime pages).
import { shellQuote } from './uptime';
import { isUuid } from './uuid';

export type Platform = 'http' | 'syslog' | 'vector' | 'docker' | 'other';

export const PLATFORMS: Platform[] = ['http', 'syslog', 'vector', 'docker', 'other'];

export const PLATFORM_LABELS: Record<Platform, string> = {
	http: 'HTTP',
	syslog: 'Syslog',
	vector: 'Vector',
	docker: 'Docker',
	other: 'Other'
};

export type Source = {
	id: string;
	project_id: string;
	name: string;
	platform: Platform;
	retention_days: number;
	daily_byte_quota: number | null;
	created_at: string;
	updated_at: string;
};

/** Create and rotate return the token once more (members and admins only). */
export type SourceWithToken = Source & { token: string };

export type DailyUsage = {
	day: string;
	events: number;
	bytes: number;
	events_dropped: number;
	events_sampled_out: number;
	requests_rate_limited: number;
	requests_over_quota: number;
};

export const MIB = 1024 * 1024;
/** Shown in snippets until a member reveals the real token. */
export const TOKEN_PLACEHOLDER = 'YOUR_SOURCE_TOKEN';

/** "512 B", "1.5 KiB", "120 MiB". */
export function formatBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	const units = ['KiB', 'MiB', 'GiB', 'TiB'];
	let value = bytes / 1024;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${value >= 100 ? Math.round(value) : Number(value.toFixed(1))} ${units[unit]}`;
}

/** A stored quota as the MiB value the form shows ('' = unlimited). */
export function quotaInMib(quota: number | null): string {
	return quota === null ? '' : String(Number((quota / MIB).toFixed(2)));
}

/** "Unlimited", or the quota per day in readable units. */
export function formatQuota(quota: number | null): string {
	return quota === null ? 'Unlimited' : `${formatBytes(quota)} / day`;
}

export function ingestUrl(engineUrl: string): string {
	return `${engineUrl}/api/v1/logs`;
}

export function curlSnippet(url: string, token: string): string {
	return [
		`curl -X POST ${shellQuote(url)} \\`,
		`  -H ${shellQuote(`Authorization: Bearer ${token}`)} \\`,
		"  -H 'Content-Type: application/json' \\",
		`  -d '{"message":"Hello from Nami","level":"info"}'`
	].join('\n');
}

function vectorSink(url: string, token: string, input: string): string[] {
	return [
		'sinks:',
		'  nami:',
		'    type: http',
		`    inputs: [${input}]`,
		`    uri: ${JSON.stringify(url)}`,
		'    compression: gzip',
		'    encoding:',
		'      codec: json',
		'    auth:',
		'      strategy: bearer',
		`      token: ${JSON.stringify(token)}`
	];
}

export function vectorSnippet(url: string, token: string): string {
	return [
		'# vector.yaml',
		'sources:',
		'  app_logs:',
		'    type: file',
		'    include: ["/var/log/app/*.log"]',
		'',
		...vectorSink(url, token, 'app_logs')
	].join('\n');
}

/** rsyslog forwarding RFC 5424 over TCP, the token in structured data. */
export function rsyslogSnippet(host: string, port: number, token: string): string {
	return [
		'# /etc/rsyslog.d/90-nami.conf',
		'template(name="NamiFormat" type="string"',
		`  string="<%pri%>1 %timestamp:::date-rfc3339% %HOSTNAME% %app-name% %procid% %msgid% [nami token=\\"${token}\\"] %msg%\\n")`,
		`*.* action(type="omfwd" target="${host}" port="${port}" protocol="tcp" template="NamiFormat")`
	].join('\n');
}

/** Every container's logs, shipped by a Vector container. */
export function dockerSnippet(url: string, token: string): string {
	return [
		'# docker-compose.yml',
		'services:',
		'  vector:',
		'    image: timberio/vector:latest-alpine',
		'    restart: unless-stopped',
		'    volumes:',
		'      - /var/run/docker.sock:/var/run/docker.sock:ro',
		'      - ./vector.yaml:/etc/vector/vector.yaml:ro',
		'',
		'# vector.yaml',
		'sources:',
		'  docker:',
		'    type: docker_logs',
		'',
		...vectorSink(url, token, 'docker')
	].join('\n');
}

// ---- Explorer -------------------------------------------------------------

export type LogLevel =
	'trace' | 'debug' | 'info' | 'notice' | 'warn' | 'error' | 'critical' | 'alert' | 'emergency' | 'fatal';

export const LEVELS: LogLevel[] = [
	'trace',
	'debug',
	'info',
	'notice',
	'warn',
	'error',
	'critical',
	'alert',
	'emergency',
	'fatal'
];

export type LogEntry = {
	id: string;
	source_id: string;
	dt: string;
	level: LogLevel;
	message: string;
	fields: unknown;
	trace_id: string;
	span_id: string;
	pattern_hash: string;
};

export type HistogramBucket = {
	start: string;
	count: number;
	errors: number;
	warnings: number;
};
export type Histogram = {
	step_seconds: number;
	total: number;
	buckets: HistogramBucket[];
};

export type TimePreset = '15m' | '1h' | '6h' | '24h' | '7d';
export const TIME_PRESETS: {
	value: TimePreset;
	label: string;
	minutes: number;
}[] = [
	{ value: '15m', label: '15 m', minutes: 15 },
	{ value: '1h', label: '1 h', minutes: 60 },
	{ value: '6h', label: '6 h', minutes: 360 },
	{ value: '24h', label: '24 h', minutes: 1440 },
	{ value: '7d', label: '7 d', minutes: 10080 }
];
export const DEFAULT_PRESET: TimePreset = '1h';

export type FilterOp =
	'eq' | 'neq' | 'contains' | 'exists' | 'regex' | 'not_regex' | 'gt' | 'gte' | 'lt' | 'lte';
export const FILTER_OPS: { value: FilterOp; label: string }[] = [
	{ value: 'eq', label: '=' },
	{ value: 'neq', label: '≠' },
	{ value: 'contains', label: 'contains' },
	{ value: 'exists', label: 'exists' },
	{ value: 'regex', label: 'matches regex' },
	{ value: 'not_regex', label: 'does not match regex' },
	{ value: 'gt', label: '>' },
	{ value: 'gte', label: '≥' },
	{ value: 'lt', label: '<' },
	{ value: 'lte', label: '≤' }
];
export type FieldFilter = { path: string; op: FilterOp; value?: string };

export const MAX_FILTERS = 10;
export const MAX_COLUMNS = 10;
export const PAGE_SIZE = 100;
/** Live rows kept in the page; older ones fall off. */
export const LIVE_ROW_CAP = 1000;

/** Everything the explorer shows, as it lives in the URL (never a token). */
export type ExplorerState = {
	range: TimePreset | 'custom';
	/** ISO 8601, only for `range: 'custom'`. */
	from: string | null;
	to: string | null;
	q: string;
	inFields: boolean;
	levels: LogLevel[];
	sources: string[];
	filters: FieldFilter[];
	columns: string[];
	live: boolean;
};

const SEGMENT = /^[A-Za-z0-9_-]{1,64}$/;

/** The engine's field path rule: 1-5 dot-separated segments of A-Z a-z 0-9 _ -. */
export function isFieldPath(path: string): boolean {
	const segments = path.split('.');
	return segments.length <= 5 && segments.every((segment) => SEGMENT.test(segment));
}

export function filterNeedsValue(op: FilterOp): boolean {
	return op !== 'exists';
}

function isoOrNull(raw: string | null): string | null {
	if (!raw) return null;
	const time = Date.parse(raw);
	return Number.isNaN(time) ? null : new Date(time).toISOString();
}

function parseFilter(raw: string): FieldFilter | null {
	try {
		const value = JSON.parse(raw);
		if (!value || typeof value.path !== 'string' || !isFieldPath(value.path)) return null;
		if (!FILTER_OPS.some((op) => op.value === value.op)) return null;
		if (!filterNeedsValue(value.op)) return { path: value.path, op: value.op };
		if (typeof value.value !== 'string') return null;
		return { path: value.path, op: value.op, value: value.value };
	} catch {
		return null;
	}
}

function unique<T>(items: T[]): T[] {
	return [...new Set(items)];
}

/** Reads the explorer URL; anything invalid is dropped rather than failing. */
export function parseExplorerState(params: URLSearchParams): ExplorerState {
	const from = isoOrNull(params.get('from'));
	const to = isoOrNull(params.get('to'));
	const rawRange = params.get('range');
	let range: ExplorerState['range'] = DEFAULT_PRESET;
	if (rawRange === 'custom' && from && to && from < to) range = 'custom';
	else if (TIME_PRESETS.some((preset) => preset.value === rawRange)) range = rawRange as TimePreset;
	return {
		range,
		from: range === 'custom' ? from : null,
		to: range === 'custom' ? to : null,
		q: params.get('q') ?? '',
		inFields: params.get('in_fields') === '1',
		levels: unique(
			params.getAll('level').filter((level): level is LogLevel => LEVELS.includes(level as LogLevel))
		),
		sources: unique(params.getAll('source').filter((id) => isUuid(id))),
		filters: params
			.getAll('filter')
			.map(parseFilter)
			.filter((filter): filter is FieldFilter => filter !== null)
			.slice(0, MAX_FILTERS),
		columns: unique(params.getAll('col').filter(isFieldPath)).slice(0, MAX_COLUMNS),
		live: params.get('live') === '1'
	};
}

/** The canonical query string of a state (defaults left out). */
export function explorerParams(state: ExplorerState): URLSearchParams {
	const params = new URLSearchParams();
	if (state.range === 'custom' && state.from && state.to) {
		params.set('range', 'custom');
		params.set('from', state.from);
		params.set('to', state.to);
	} else if (state.range !== DEFAULT_PRESET) {
		params.set('range', state.range);
	}
	if (state.q) params.set('q', state.q);
	if (state.inFields) params.set('in_fields', '1');
	for (const level of state.levels) params.append('level', level);
	for (const source of state.sources) params.append('source', source);
	for (const filter of state.filters) params.append('filter', JSON.stringify(filter));
	for (const column of state.columns) params.append('col', column);
	if (state.live) params.set('live', '1');
	return params;
}

export function explorerHref(path: string, state: ExplorerState): string {
	const query = explorerParams(state).toString();
	return query ? `${path}?${query}` : path;
}

/** The absolute window: a preset counts back from `now`. */
export function resolveWindow(state: ExplorerState, now: number): { from: string; to: string } {
	if (state.range === 'custom' && state.from && state.to) return { from: state.from, to: state.to };
	const minutes = TIME_PRESETS.find((preset) => preset.value === state.range)?.minutes ?? 60;
	return {
		from: new Date(now - minutes * 60_000).toISOString(),
		to: new Date(now).toISOString()
	};
}

/** The filters as nami-engine's query parameters (no window). */
export function tailParams(state: ExplorerState): URLSearchParams {
	const params = new URLSearchParams();
	for (const source of state.sources) params.append('source_id', source);
	for (const level of state.levels) params.append('level', level);
	if (state.q) params.set('q', state.q);
	if (state.q && state.inFields) params.set('in_fields', 'true');
	for (const filter of state.filters) params.append('filter', JSON.stringify(filter));
	return params;
}

/** Search and histogram parameters: the filters plus the window. */
export function engineParams(state: ExplorerState, window: { from: string; to: string }): URLSearchParams {
	const params = tailParams(state);
	params.set('from', window.from);
	params.set('to', window.to);
	return params;
}

/** The value at a dot path inside a row's fields (`undefined` when absent). */
export function fieldAt(fields: unknown, path: string): unknown {
	let current: unknown = fields;
	for (const segment of path.split('.')) {
		if (current === null || typeof current !== 'object' || Array.isArray(current)) return undefined;
		current = (current as Record<string, unknown>)[segment];
	}
	return current;
}

/** A field value for a table cell: strings as they are, the rest as JSON. */
export function formatFieldValue(value: unknown): string {
	if (value === undefined) return '';
	if (typeof value === 'string') return value;
	return JSON.stringify(value);
}

/** "2026-10-13 14:30:05.123" (UTC, milliseconds). */
export function formatLogTime(iso: string): string {
	return new Date(iso).toISOString().slice(0, 23).replace('T', ' ');
}

/** Badge classes per level; the badge text always names the level. */
export function levelClasses(level: string): string {
	switch (level) {
		case 'trace':
			return 'bg-neutral-500/10 text-neutral-500';
		case 'debug':
			return 'bg-neutral-400/10 text-neutral-400';
		case 'info':
			return 'bg-sky-400/10 text-sky-300';
		case 'notice':
			return 'bg-teal-400/10 text-teal-300';
		case 'warn':
			return 'bg-amber-400/10 text-amber-300';
		case 'error':
			return 'bg-red-500/10 text-red-300';
		case 'critical':
		case 'alert':
		case 'emergency':
		case 'fatal':
			return 'bg-rose-500/20 font-semibold text-rose-200';
		default:
			return 'bg-neutral-500/10 text-neutral-400';
	}
}

/** A copy of `list` with `item` added, or removed when present. */
export function toggled<T>(list: T[], item: T): T[] {
	return list.includes(item) ? list.filter((other) => other !== item) : [...list, item];
}

/** `datetime-local` input value (UTC) <-> ISO. */
export function toInputValue(iso: string | null): string {
	return iso ? iso.slice(0, 16) : '';
}

export function fromInputValue(value: string): string | null {
	return value ? isoOrNull(`${value}:00Z`) : null;
}

/** A histogram bucket split into the stacked parts: errors, warnings, the rest. */
export function bucketParts(bucket: HistogramBucket): {
	errors: number;
	warnings: number;
	other: number;
} {
	return {
		errors: bucket.errors,
		warnings: bucket.warnings,
		other: Math.max(0, bucket.count - bucket.errors - bucket.warnings)
	};
}

/** The end of a bucket (exclusive): its start plus one step. */
export function bucketEnd(bucket: HistogramBucket, stepSeconds: number): string {
	return new Date(Date.parse(bucket.start) + stepSeconds * 1000).toISOString();
}
