// Shapes of nami-engine's log alerts (`/projects/{id}/log-alerts`) and the
// pure helpers their pages share. Times render in UTC.
import { parseExplorerState, type FieldFilter, type LogLevel } from './logs';

/** The engine's `LogQuery`: empty parts are left out (it denies unknown keys). */
export type LogAlertQuery = {
	q?: string;
	in_fields?: boolean;
	levels?: LogLevel[];
	source_ids?: string[];
	filters?: FieldFilter[];
	pattern_hashes?: string[];
};

export type Comparison = 'gt' | 'lt';

export type LogAlert = {
	id: string;
	name: string;
	query: LogAlertQuery;
	threshold: number;
	comparison: Comparison;
	window_seconds: number;
	interval_seconds: number;
	enabled: boolean;
	state: 'ok' | 'alerting';
	last_evaluated_at: string | null;
	last_count: number | null;
	last_error: string | null;
	last_error_at: string | null;
	next_evaluation_at: string;
	/** null: the project default. */
	escalation_policy_id?: string | null;
	severity_id?: string | null;
};

/** What the alert form edits: the alert's settings without its evaluation state. */
export type LogAlertSettings = Pick<
	LogAlert,
	| 'name'
	| 'query'
	| 'threshold'
	| 'comparison'
	| 'window_seconds'
	| 'interval_seconds'
	| 'enabled'
	| 'escalation_policy_id'
	| 'severity_id'
>;

export const WINDOW_RANGE = { min: 60, max: 86_400 };
export const INTERVAL_RANGE = { min: 60, max: 86_400 };
export const MAX_THRESHOLD = 1_000_000_000_000;

export const WINDOW_PRESETS: { seconds: number; label: string }[] = [60, 300, 900, 3600, 21_600, 86_400].map(
	(seconds) => ({ seconds, label: formatSeconds(seconds) })
);
export const INTERVAL_PRESETS: { seconds: number; label: string }[] = [60, 300, 900, 3600].map((seconds) => ({
	seconds,
	label: formatSeconds(seconds)
}));

export const NEW_ALERT: LogAlertSettings = {
	name: '',
	query: {},
	threshold: 0,
	comparison: 'gt',
	window_seconds: 300,
	interval_seconds: 60,
	enabled: true,
	escalation_policy_id: null,
	severity_id: null
};

/** "5 min", "1 h", "90 s" (as the engine's alert emails write it). */
export function formatSeconds(seconds: number): string {
	if (seconds % 3600 === 0) return `${seconds / 3600} h`;
	if (seconds % 60 === 0) return `${seconds / 60} min`;
	return `${seconds} s`;
}

/** "more than 10 matching events in 5 min". */
export function describeCondition(alert: Pick<LogAlert, 'comparison' | 'threshold' | 'window_seconds'>): string {
	const word = alert.comparison === 'gt' ? 'more than' : 'fewer than';
	const events = alert.threshold === 1 ? 'event' : 'events';
	return `${word} ${alert.threshold.toLocaleString('en-US')} matching ${events} in ${formatSeconds(alert.window_seconds)}`;
}

/** Whether `count` meets the alert's condition (the engine's `gt`/`lt`). */
export function conditionMet(alert: Pick<LogAlert, 'comparison' | 'threshold'>, count: number): boolean {
	return alert.comparison === 'gt' ? count > alert.threshold : count < alert.threshold;
}

/**
 * An alert query prefilled from an explorer link. The URL rules check every
 * part; the window, columns, tab and live tail are not part of an alert.
 */
export function alertQueryFromExplorer(params: URLSearchParams): LogAlertQuery {
	const state = parseExplorerState(params);
	const query: LogAlertQuery = {};
	if (state.q) query.q = state.q;
	if (state.inFields) query.in_fields = true;
	if (state.levels.length) query.levels = state.levels;
	if (state.sources.length) query.source_ids = state.sources;
	if (state.filters.length) query.filters = state.filters;
	if (state.patterns.length) query.pattern_hashes = state.patterns;
	return query;
}
