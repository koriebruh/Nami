// Types of the engine's incident API (`GET /projects/{id}/incidents` and the
// routes under it). Shared by server loaders and components.

import { isUuid } from '#lib/core/uuid';

export type IncidentStatus = 'open' | 'acknowledged' | 'resolved';

/** One row of the incident list (the engine's `IncidentListItem`). */
export type Incident = {
	id: string;
	project_id: string;
	monitor_id: string | null;
	heartbeat_id: string | null;
	log_alert_id: string | null;
	source: string;
	/** Name of the monitor, heartbeat or log alert the incident belongs to. */
	source_name: string | null;
	title: string;
	cause: string | null;
	status: IncidentStatus;
	severity: string | null;
	severity_rank: number | null;
	started_at: string;
	acknowledged_at: string | null;
	acknowledged_by_email: string | null;
	resolved_at: string | null;
	/** Job output a failing heartbeat reported; plain text, already truncated by the engine. */
	details: string | null;
};

/** One page of the incident list, newest first. */
export type IncidentPage = { incidents: Incident[]; next_cursor: string | null };

/** One timeline event; `message` and `metadata` are data, never markup. */
export type IncidentEvent = {
	id: number;
	kind: string;
	actor_id: string | null;
	actor_email: string | null;
	message: string | null;
	metadata: Record<string, unknown>;
	created_at: string;
};

/** One incident with its latest timeline events, oldest first. */
export type IncidentDetail = Incident & {
	severity_id: string | null;
	metadata: Record<string, unknown>;
	/** Markdown source; rendered only through the escaping renderer. */
	postmortem: string | null;
	resolved_by_email: string | null;
	created_by_email: string | null;
	events: IncidentEvent[];
};

/** One of the project's severities; `rank` 1 is the most severe. */
export type Severity = { id: string; name: string; rank: number; is_default: boolean };

export const STATUSES = ['open', 'acknowledged', 'resolved'] as const;
export const SOURCES = ['monitor', 'heartbeat', 'log_alert', 'metric_alert', 'manual', 'integration', 'report_form'] as const;

/** The incident list's tabs; `unresolved` (open + acknowledged) is the default. */
export const LIST_TABS = ['unresolved', 'acknowledged', 'resolved', 'all'] as const;
export type ListTab = (typeof LIST_TABS)[number];
export type ListQuery = { tab: ListTab; source: string | null; cursor: string | null };

const CURSOR = /^[A-Za-z0-9_-]{1,200}$/;

/** The list's query string, keeping only known values: nothing else reaches the engine. */
export function parseListQuery(url: URL): ListQuery {
	const tab = url.searchParams.get('tab');
	const source = url.searchParams.get('source');
	const cursor = url.searchParams.get('cursor');
	return {
		tab: LIST_TABS.find((t) => t === tab) ?? 'unresolved',
		source: SOURCES.find((s) => s === source) ?? null,
		cursor: cursor !== null && CURSOR.test(cursor) ? cursor : null
	};
}

/** The engine list query (`?status=..&source=..&cursor=..`) of a parsed list query. */
export function engineListQuery(q: ListQuery): string {
	const params = new URLSearchParams();
	if (q.tab !== 'all') params.set('status', q.tab);
	if (q.source) params.set('source', q.source);
	if (q.cursor) params.set('cursor', q.cursor);
	const query = params.toString();
	return query ? `?${query}` : '';
}

const EVENT_LABELS: Record<string, string> = {
	opened: 'Incident opened',
	acknowledged: 'Acknowledged',
	resolved: 'Resolved',
	reopened: 'Reopened',
	escalated: 'Escalated',
	notification_sent: 'Notification sent',
	notification_failed: 'Notification failed',
	comment: 'Comment',
	postmortem_updated: 'Postmortem updated',
	integration_event: 'Integration event',
	step_skipped: 'Escalation step skipped',
	escalation_fallback: 'Moved to fallback policy',
	escalation_exhausted: 'Escalation exhausted',
	severity_changed: 'Severity changed',
	metadata_updated: 'Metadata updated',
	ticket_created: 'Ticket created'
};

/** A timeline event kind as people read it; an unknown kind as is. */
export function eventLabel(kind: string): string {
	return Object.hasOwn(EVENT_LABELS, kind) ? EVENT_LABELS[kind] : kind;
}

/** How long an incident lasted (or has lasted, until `now`): "45 s", "12 min", "3 h 12 min", "2 d 4 h". */
export function duration(startedAt: string, endedAt: string | null, now: Date): string {
	const end = endedAt ? Date.parse(endedAt) : now.getTime();
	const seconds = Math.max(0, Math.floor((end - Date.parse(startedAt)) / 1000));
	const days = Math.floor(seconds / 86_400);
	const hours = Math.floor((seconds % 86_400) / 3_600);
	const minutes = Math.floor((seconds % 3_600) / 60);
	if (days > 0) return hours > 0 ? `${days} d ${hours} h` : `${days} d`;
	if (hours > 0) return minutes > 0 ? `${hours} h ${minutes} min` : `${hours} h`;
	if (minutes > 0) return `${minutes} min`;
	return `${seconds} s`;
}

/** The page of the monitor, heartbeat or log alert an incident belongs to; null for other sources. */
export function sourceLink(projectId: string, i: Pick<Incident, 'monitor_id' | 'heartbeat_id' | 'log_alert_id'>): string | null {
	if (isUuid(i.monitor_id)) return `/projects/${projectId}/monitors/${i.monitor_id}`;
	if (isUuid(i.heartbeat_id)) return `/projects/${projectId}/heartbeats/${i.heartbeat_id}`;
	if (isUuid(i.log_alert_id)) return `/projects/${projectId}/logs/alerts/${i.log_alert_id}`;
	return null;
}

/** One escalation policy, as the "New incident" form lists it. */
export type PolicyOption = { id: string; name: string; is_default: boolean };

/** Label and badge class of each status; acknowledged is still unresolved. */
export const STATUS_BADGE: Record<IncidentStatus, { label: string; class: string }> = {
	open: { label: 'Open', class: 'badge-danger' },
	acknowledged: { label: 'Acknowledged', class: 'badge-warning' },
	resolved: { label: 'Resolved', class: 'badge-neutral' }
};

export const SOURCE_LABELS: Record<(typeof SOURCES)[number], string> = {
	monitor: 'Monitor',
	heartbeat: 'Heartbeat',
	log_alert: 'Log alert',
	metric_alert: 'Metric alert',
	manual: 'Manual',
	integration: 'Integration',
	report_form: 'Report form'
};

/** The name of the incident's monitor, heartbeat or log alert, else its source kind. */
export function sourceLabel(i: Pick<Incident, 'source' | 'source_name'>): string {
	if (i.source_name) return i.source_name;
	return Object.hasOwn(SOURCE_LABELS, i.source) ? SOURCE_LABELS[i.source as (typeof SOURCES)[number]] : i.source;
}
