// Types of the engine's on-call API (`/projects/{id}/schedules`, `/on-call`)
// and the time helpers the schedule pages use. Schedule times are shown in
// the schedule's own zone (the UTC instant in a tooltip); override inputs are
// typed in that zone and converted here.

export type Restriction = { weekdays: number[]; from: string; to: string };
export type Participant = { user_id: string; email: string; is_member: boolean };
export type OnCallNow = { user_id: string; email: string; until: string };

export type Schedule = {
	id: string;
	project_id: string;
	name: string;
	timezone: string;
	rotation: 'daily' | 'weekly' | 'custom';
	rotation_hours: number | null;
	/** "HH:MM", local to `timezone`. */
	handoff_time: string;
	/** "YYYY-MM-DD". */
	start_date: string;
	restrictions: Restriction[];
	participants: Participant[];
	on_call_now: OnCallNow | null;
};

/** One merged shift; `override_id` set when an override covers it. */
export type Shift = {
	user_id: string;
	email: string;
	is_member: boolean;
	starts_at: string;
	ends_at: string;
	override_id: string | null;
};

export type Override = { id: string; user_id: string; email: string; starts_at: string; ends_at: string };

/** One row of the dashboard's "on call now" card. */
export type BoardEntry = { schedule_id: string; name: string; on_call_now: OnCallNow | null };

export type Member = { user_id: string; email: string; project_role: string };

// Month and weekday names are our own: ICU's en-GB names vary by version
// (September is "Sept" in recent ones).
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const WEEKDAY_NAMES = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

/** The wall clock of `instant` in `timeZone`, read as if it were UTC (ms);
 *  throws a RangeError for an unknown zone. */
function wallAsUtc(instant: number, timeZone: string): number {
	const parts = new Intl.DateTimeFormat('en-GB', {
		timeZone,
		year: 'numeric',
		month: 'numeric',
		day: 'numeric',
		hour: 'numeric',
		minute: 'numeric',
		second: 'numeric',
		hourCycle: 'h23'
	}).formatToParts(new Date(instant));
	const get = (type: Intl.DateTimeFormatPartTypes) => Number(parts.find((p) => p.type === type)?.value);
	return Date.UTC(get('year'), get('month') - 1, get('day'), get('hour'), get('minute'), get('second'));
}

const LOCAL_DATETIME = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2}))?$/;
const DAY = 86_400_000;

/**
 * A `datetime-local` value ("2026-10-19T09:00") in `timezone` as an ISO UTC
 * instant. A repeated (fall-back) time gives the earlier instant; a skipped
 * (spring-forward) time, an impossible date or an unknown zone gives null.
 */
export function localInputToUtc(local: string, timezone: string): string | null {
	const m = LOCAL_DATETIME.exec(local);
	if (!m) return null;
	const [, y, mo, d, h, mi, s] = m.map(Number);
	const wall = Date.UTC(y, mo - 1, d, h, mi, Number.isNaN(s) ? 0 : s);
	// An impossible date or time ("02-30", "25:00") rolls over in Date.UTC.
	if (new Date(wall).toISOString().slice(0, 19) !== `${m[1]}-${m[2]}-${m[3]}T${m[4]}:${m[5]}:${m[6] ?? '00'}`) return null;
	try {
		// The zone's offset near `wall` is one of those a day apart around it;
		// keep the instants that read back as the same wall time.
		const candidates = [wall - DAY, wall, wall + DAY]
			.map((t) => wall - (wallAsUtc(t, timezone) - t))
			.filter((t) => wallAsUtc(t, timezone) === wall);
		if (candidates.length === 0) return null;
		return new Date(Math.min(...candidates)).toISOString();
	} catch {
		return null;
	}
}

/** "Mon 19 Oct 09:00" in `timezone`, or in UTC when the runtime does not know the zone. */
export function formatInZone(iso: string, timezone: string): string {
	const instant = new Date(iso).getTime();
	let wall: Date;
	try {
		wall = new Date(wallAsUtc(instant, timezone));
	} catch {
		wall = new Date(instant);
	}
	const two = (n: number) => String(n).padStart(2, '0');
	return `${WEEKDAY_NAMES[wall.getUTCDay()]} ${wall.getUTCDate()} ${MONTHS[wall.getUTCMonth()]} ${two(wall.getUTCHours())}:${two(wall.getUTCMinutes())}`;
}

/** The shift list's rows, in the schedule's zone. */
export function shiftRows(
	shifts: Pick<Shift, 'user_id' | 'email' | 'starts_at' | 'ends_at' | 'override_id'>[],
	timezone: string
): { who: string; from: string; to: string; isOverride: boolean }[] {
	return shifts.map((s) => ({
		who: s.email,
		from: formatInZone(s.starts_at, timezone),
		to: formatInZone(s.ends_at, timezone),
		isOverride: s.override_id !== null
	}));
}
