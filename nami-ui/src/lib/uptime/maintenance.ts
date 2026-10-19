// Shapes of nami-engine's maintenance-window JSON plus pure display
// helpers. Like the rest of the UI, one-off windows are entered and shown in
// UTC; weekly windows carry their own IANA time zone.
import { formatUtc } from './uptime';

export type WindowKind = 'once' | 'weekly';

export type MaintenanceWindow = {
	id: string;
	project_id: string;
	/** `null`: every monitor and heartbeat of the project. */
	monitor_id: string | null;
	title: string;
	kind: WindowKind;
	starts_at: string | null;
	ends_at: string | null;
	/** ISO weekdays, 1 = Monday ... 7 = Sunday. */
	days_of_week: number[] | null;
	/** "HH:MM:SS". */
	start_time: string | null;
	end_time: string | null;
	timezone: string | null;
	created_at: string;
	/** In effect right now. */
	active: boolean;
};

export const WEEKDAYS = [
	{ iso: 1, label: 'Mon' },
	{ iso: 2, label: 'Tue' },
	{ iso: 3, label: 'Wed' },
	{ iso: 4, label: 'Thu' },
	{ iso: 5, label: 'Fri' },
	{ iso: 6, label: 'Sat' },
	{ iso: 7, label: 'Sun' }
] as const;

/** "22:00:00" -> "22:00". */
export function shortTime(time: string | null): string {
	return (time ?? '').slice(0, 5);
}

function describeDays(days: number[]): string {
	const sorted = [...days].sort((a, b) => a - b);
	if (sorted.length === 7) return 'Every day';
	if (sorted.join() === '1,2,3,4,5') return 'Weekdays';
	if (sorted.join() === '6,7') return 'Weekends';
	return sorted.map((day) => WEEKDAYS[day - 1]?.label ?? `day ${day}`).join(', ');
}

/** One line saying when the window applies. */
export function describeSchedule(
	w: Pick<MaintenanceWindow, 'kind' | 'starts_at' | 'ends_at' | 'days_of_week' | 'start_time' | 'end_time' | 'timezone'>
): string {
	if (w.kind === 'once') {
		return `${w.starts_at ? formatUtc(w.starts_at) : '?'} → ${w.ends_at ? formatUtc(w.ends_at) : '?'}`;
	}
	const start = shortTime(w.start_time);
	const end = shortTime(w.end_time);
	const overnight = end <= start ? ' (next day)' : '';
	return `${describeDays(w.days_of_week ?? [])}, ${start}–${end}${overnight} ${w.timezone ?? ''}`.trim();
}

/** What the window covers, in words. */
export function describeScope(monitorId: string | null, monitors: { id: string; name: string }[]): string {
	if (!monitorId) return 'All monitors and heartbeats';
	return monitors.find((m) => m.id === monitorId)?.name ?? 'One monitor';
}

/** An engine timestamp as a `datetime-local` value in UTC ("2026-10-13T22:00"). */
export function toUtcInputValue(iso: string | null): string {
	if (!iso) return '';
	const date = new Date(iso);
	return Number.isNaN(date.getTime()) ? '' : date.toISOString().slice(0, 16);
}
