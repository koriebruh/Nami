// Availability (SLA) ranges and display helpers for the monitor and heartbeat
// pages. The engine's range cap is 1096 days; its export cap is 180 days.

/** nami-engine's availability JSON. `uptime_pct` is 0-100, null without data. */
export type Availability = {
	from: string;
	to: string;
	total_seconds: number;
	maintenance_seconds: number;
	downtime_seconds: number;
	uptime_pct: number | null;
	incidents: number;
	longest_incident_seconds: number;
};

export type RangeKey = '24h' | '7d' | '30d' | '90d' | 'custom';

export const RANGE_PRESETS: { key: Exclude<RangeKey, 'custom'>; label: string; days: number }[] = [
	{ key: '24h', label: '24h', days: 1 },
	{ key: '7d', label: '7d', days: 7 },
	{ key: '30d', label: '30d', days: 30 },
	{ key: '90d', label: '90d', days: 90 }
];
export const DEFAULT_RANGE: RangeKey = '30d';
export const MAX_RANGE_DAYS = 1096;
export const MAX_EXPORT_DAYS = 180;

const DAY_MS = 86_400_000;

/** The range a page shows. `from` and `to` are null for a custom range not yet picked. */
export type SelectedRange = {
	key: RangeKey;
	from: string | null;
	to: string | null;
	/** The custom dates as typed (YYYY-MM-DD), to refill the form. */
	customFrom: string;
	customTo: string;
	/** Why a custom range was refused. */
	error: string | null;
};

const DATE = /^\d{4}-\d{2}-\d{2}$/;

function utcMidnight(value: string | null): number | null {
	if (!value || !DATE.test(value)) return null;
	const ms = Date.parse(`${value}T00:00:00.000Z`);
	// Date.parse rolls 2026-02-30 over to March: require the same date back.
	if (!Number.isFinite(ms) || new Date(ms).toISOString().slice(0, 10) !== value) return null;
	return ms;
}

/**
 * `YYYY-MM-DD` dates (UTC midnight; `to` inclusive, so the next midnight),
 * `from` before `to`, at most 1096 days; else null.
 */
export function customRange(from: string | null, to: string | null): { from: string; to: string } | null {
	const start = utcMidnight(from);
	const end = utcMidnight(to);
	if (start === null || end === null) return null;
	const stop = end + DAY_MS;
	if (start >= stop || stop - start > MAX_RANGE_DAYS * DAY_MS) return null;
	return { from: new Date(start).toISOString(), to: new Date(stop).toISOString() };
}

/** The range the query string names (`range`, and `from`/`to` for custom). */
export function selectRange(params: URLSearchParams, now: Date): SelectedRange {
	const requested = params.get('range');
	const preset = RANGE_PRESETS.find((p) => p.key === requested);
	const blank = { customFrom: '', customTo: '', error: null };
	if (requested !== 'custom') {
		const chosen = preset ?? RANGE_PRESETS.find((p) => p.key === DEFAULT_RANGE)!;
		return {
			key: chosen.key,
			from: new Date(now.getTime() - chosen.days * DAY_MS).toISOString(),
			to: now.toISOString(),
			...blank
		};
	}
	const customFrom = params.get('from') ?? '';
	const customTo = params.get('to') ?? '';
	if (!customFrom && !customTo) return { key: 'custom', from: null, to: null, customFrom, customTo, error: null };
	const range = customRange(customFrom, customTo);
	if (!range) {
		return {
			key: 'custom',
			from: null,
			to: null,
			customFrom,
			customTo,
			error: `Pick two valid dates, the first not after the second, at most ${MAX_RANGE_DAYS} days apart.`
		};
	}
	return { key: 'custom', ...range, customFrom, customTo, error: null };
}

/** The range to export: `range` clipped to its last 180 days, null without one. */
export function exportRange(range: { from: string | null; to: string | null }): {
	from: string;
	to: string;
	clipped: boolean;
} | null {
	if (!range.from || !range.to) return null;
	const to = Date.parse(range.to);
	const from = Date.parse(range.from);
	const earliest = to - MAX_EXPORT_DAYS * DAY_MS;
	const clipped = from < earliest;
	return { from: new Date(clipped ? earliest : from).toISOString(), to: range.to, clipped };
}

/**
 * Uptime as `99.950`, three decimals truncated so downtime never rounds away
 * (99.9996 is "99.999", not "100"); exactly 100 is "100". Null is "No data":
 * a range without checks is not 0 %.
 */
export function formatPct(pct: number | null): string {
	if (pct === null || !Number.isFinite(pct)) return 'No data';
	if (pct >= 100) return '100';
	const thousandths = Math.floor(Math.max(0, pct) * 1000 + 1e-7);
	return (thousandths / 1000).toFixed(3);
}
