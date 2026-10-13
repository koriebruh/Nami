// Shapes of nami-engine's uptime JSON, plus pure display helpers shared by
// the monitor and incident pages. All times render in UTC so server- and
// client-rendered markup agree (and match the alert emails).

export type Status = 'pending' | 'up' | 'down';
export type CheckType = 'http' | 'keyword' | 'tcp';

export type Monitor = {
	id: string;
	project_id: string;
	name: string;
	check_type: CheckType;
	url: string | null;
	host: string | null;
	port: number | null;
	http_method: 'GET' | 'HEAD';
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

export type Incident = {
	id: string;
	project_id: string;
	monitor_id: string | null;
	heartbeat_id: string | null;
	source_name: string | null;
	title: string;
	cause: string | null;
	status: 'open' | 'resolved';
	started_at: string;
	resolved_at: string | null;
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
};

/** The URL a cron job pings: straight to nami-engine, not through this BFF. */
export function pingUrl(engineUrl: string, token: string): string {
	return `${engineUrl.replace(/\/+$/, '')}/api/v1/heartbeat/${encodeURIComponent(token)}`;
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

/** What the monitor checks: the URL, or host:port for TCP. */
export function monitorTarget(m: Pick<Monitor, 'check_type' | 'url' | 'host' | 'port'>): string {
	return m.check_type === 'tcp' ? `${m.host ?? ''}:${m.port ?? ''}` : (m.url ?? '');
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
 * state-change timestamp, so derive it from incidents: down since the open
 * incident started; up since the latest recovery (or creation, if it never
 * went down); pending since creation.
 */
export function statusSince(
	monitor: Pick<Monitor, 'current_status' | 'created_at'>,
	incidents: Incident[]
): string {
	if (monitor.current_status === 'down') {
		return incidents.find((i) => i.status === 'open')?.started_at ?? monitor.created_at;
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
