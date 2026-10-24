// Types of the engine's incident API (`GET /projects/{id}/incidents` and the
// routes under it). Shared by server loaders and components.

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
