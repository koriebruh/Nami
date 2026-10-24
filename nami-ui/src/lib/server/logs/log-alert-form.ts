import { isUuid } from '#lib/core/uuid';
import {
	INTERVAL_RANGE,
	MAX_THRESHOLD,
	WINDOW_RANGE,
	type Comparison,
	type LogAlertQuery
} from '#lib/logs/log-alerts';
import { isPatternHash, LEVELS, MAX_FILTERS, MAX_PATTERNS, parseFilter, type FieldFilter, type LogLevel } from '#lib/logs/logs';

export type LogAlertPayload = {
	name: string;
	query: LogAlertQuery;
	threshold: number;
	comparison: Comparison;
	window_seconds: number;
	interval_seconds: number;
	enabled: boolean;
};

type Result = { ok: true; payload: LogAlertPayload } | { ok: false; message: string };

/** A single-valued field: the last value sent wins. */
function last(form: FormData, field: string): string {
	return String(form.getAll(field).at(-1) ?? '').trim();
}

function inRange(raw: string, range: { min: number; max: number }): number | null {
	if (!/^\d+$/.test(raw)) return null;
	const value = Number(raw);
	return value >= range.min && value <= range.max ? value : null;
}

/**
 * Alert form -> nami-engine JSON body (create, and the whole body on edit).
 * Invalid input is refused rather than dropped: an alert must count exactly
 * what the form showed. The engine validates everything again.
 */
export function logAlertPayloadFromForm(form: FormData): Result {
	const name = last(form, 'name');
	const length = [...name].length;
	if (length < 1 || length > 200) return { ok: false, message: 'The name must be 1 to 200 characters.' };

	const comparison = last(form, 'comparison');
	if (comparison !== 'gt' && comparison !== 'lt') return { ok: false, message: 'Pick "more than" or "fewer than".' };

	const threshold = inRange(last(form, 'threshold'), { min: 0, max: MAX_THRESHOLD });
	if (threshold === null) {
		return { ok: false, message: 'The threshold must be a whole number between 0 and 1,000,000,000,000.' };
	}
	if (comparison === 'lt' && threshold < 1) return { ok: false, message: '"Fewer than" needs a threshold of at least 1.' };

	const window = inRange(last(form, 'window_seconds'), WINDOW_RANGE);
	if (window === null) return { ok: false, message: 'The window must be between 1 minute and 24 hours.' };
	const interval = inRange(last(form, 'interval_seconds'), INTERVAL_RANGE);
	if (interval === null) return { ok: false, message: 'The check interval must be between 1 minute and 24 hours.' };

	const query: LogAlertQuery = {};
	const q = last(form, 'q');
	if (q) query.q = q;
	if (form.has('in_fields')) query.in_fields = true;

	const levels = form.getAll('level').map(String);
	if (!levels.every((level) => LEVELS.includes(level as LogLevel))) return { ok: false, message: 'Pick levels from the list.' };
	if (levels.length) query.levels = [...new Set(levels)] as LogLevel[];

	const sources = form.getAll('source').map(String);
	if (!sources.every(isUuid)) return { ok: false, message: 'Pick sources from the list.' };
	if (sources.length) query.source_ids = [...new Set(sources)];

	const rawFilters = form.getAll('filter').map(String);
	if (rawFilters.length > MAX_FILTERS) return { ok: false, message: `An alert may have at most ${MAX_FILTERS} field filters.` };
	const filters = rawFilters.map(parseFilter);
	if (filters.some((filter) => filter === null)) return { ok: false, message: 'A field filter is not valid.' };
	if (filters.length) query.filters = filters as FieldFilter[];

	const patterns = form.getAll('pattern').map(String);
	if (patterns.length > MAX_PATTERNS || !patterns.every(isPatternHash)) {
		return { ok: false, message: 'A pattern filter is not valid.' };
	}
	if (patterns.length) query.pattern_hashes = [...new Set(patterns)];

	return {
		ok: true,
		payload: {
			name,
			query,
			threshold,
			comparison,
			window_seconds: window,
			interval_seconds: interval,
			enabled: form.has('enabled')
		}
	};
}
