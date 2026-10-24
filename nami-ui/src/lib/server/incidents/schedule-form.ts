// On-call schedule and override forms -> nami-engine JSON bodies. The engine
// validates everything again (zone, HH:MM, start date, override length) and
// its message is shown; these checks catch what would otherwise be a bare
// 422 or a path built from junk.
import type { RequestEvent } from '@sveltejs/kit';
import { callEngine, enginePath, redirectToLoginIfUnauthorized } from '#lib/server/core/engine';
import { isUuid } from '#lib/core/uuid';
import { localInputToUtc, type BoardEntry, type Member, type Restriction } from '#lib/incidents/oncall';

export type SchedulePayload = {
	name: string;
	timezone: string;
	rotation: 'daily' | 'weekly' | 'custom';
	rotation_hours?: number;
	handoff_time: string;
	start_date: string;
	participants: string[];
	restrictions: Restriction[];
};

type Result<T> = { ok: true; payload: T } | { ok: false; message: string };

const MAX_PARTICIPANTS = 50;
const MAX_RESTRICTIONS = 14;

const text = (form: FormData, name: string) => String(form.get(name) ?? '').trim();

export function schedulePayloadFromForm(form: FormData): Result<SchedulePayload> {
	const name = text(form, 'name');
	if (name === '') return { ok: false, message: 'Name is required.' };
	const rotation = text(form, 'rotation');
	if (rotation !== 'daily' && rotation !== 'weekly' && rotation !== 'custom') {
		return { ok: false, message: 'Pick a rotation.' };
	}
	const payload: SchedulePayload = {
		name,
		timezone: text(form, 'timezone'),
		rotation,
		handoff_time: text(form, 'handoff_time'),
		start_date: text(form, 'start_date'),
		participants: [],
		restrictions: []
	};
	// The engine refuses rotation_hours on a daily or weekly rotation.
	if (rotation === 'custom') {
		const hours = text(form, 'rotation_hours');
		if (!/^\d{1,4}$/.test(hours) || Number(hours) < 1 || Number(hours) > 8760) {
			return { ok: false, message: 'Enter the rotation length in hours (1-8760)' };
		}
		payload.rotation_hours = Number(hours);
	}

	const participants = form.getAll('participant').map((p) => String(p).trim());
	if (participants.length < 1 || participants.length > MAX_PARTICIPANTS) {
		return { ok: false, message: `Add 1 to ${MAX_PARTICIPANTS} participants.` };
	}
	if (!participants.every(isUuid)) return { ok: false, message: 'Pick participants from the member list.' };
	if (new Set(participants.map((p) => p.toLowerCase())).size !== participants.length) {
		return { ok: false, message: 'Each participant can be listed once.' };
	}
	payload.participants = participants;

	const countText = text(form, 'restriction_count') || '0';
	const count = /^\d{1,2}$/.test(countText) ? Number(countText) : Infinity;
	if (count > MAX_RESTRICTIONS) return { ok: false, message: `At most ${MAX_RESTRICTIONS} restrictions.` };
	for (let i = 0; i < count; i++) {
		const weekdays = form.getAll(`restriction_${i}_weekday`).map((d) => String(d));
		if (weekdays.length === 0) return { ok: false, message: `Pick at least one day for restriction ${i + 1}.` };
		if (!weekdays.every((d) => /^[1-7]$/.test(d))) return { ok: false, message: `Unknown day in restriction ${i + 1}.` };
		payload.restrictions.push({
			weekdays: weekdays.map(Number),
			from: text(form, `restriction_${i}_from`),
			to: text(form, `restriction_${i}_to`)
		});
	}
	return { ok: true, payload };
}

/** `starts_at`/`ends_at` are `datetime-local` values in the schedule's `timezone`. */
export function overridePayloadFromForm(
	form: FormData,
	timezone: string
): Result<{ user_id: string; starts_at: string; ends_at: string }> {
	const userId = text(form, 'user_id');
	if (!isUuid(userId)) return { ok: false, message: 'Pick who takes over from the member list.' };
	const startsText = text(form, 'starts_at');
	const endsText = text(form, 'ends_at');
	if (!localInputToUtc(startsText, 'UTC') || !localInputToUtc(endsText, 'UTC')) {
		return { ok: false, message: 'Enter a start and an end date and time.' };
	}
	const startsAt = localInputToUtc(startsText, timezone);
	const endsAt = localInputToUtc(endsText, timezone);
	if (!startsAt || !endsAt) {
		return { ok: false, message: `That time does not exist in ${timezone} (daylight saving change)` };
	}
	if (endsAt <= startsAt) return { ok: false, message: 'The override must end after it starts.' };
	return { ok: true, payload: { user_id: userId, starts_at: startsAt, ends_at: endsAt } };
}

/** The project's members, for the participant and override pickers. They
 *  only fill the pickers, so a failure leaves them empty and the page opens. */
export async function loadMembers(event: RequestEvent): Promise<Member[]> {
	const response = await callEngine(event, enginePath`/projects/${event.params.id}/members`);
	redirectToLoginIfUnauthorized(response);
	if (!response.ok) return [];
	const members: unknown = await response.json().catch(() => []);
	return Array.isArray(members) ? (members as Member[]) : [];
}

/** The dashboard's "on call now" rows. The card is extra: any failure,
 *  an unreachable engine included, hides it and never fails the page. */
export async function loadOnCall(event: RequestEvent): Promise<BoardEntry[]> {
	try {
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/on-call`);
		if (!response.ok) return [];
		const entries: unknown = await response.json();
		return Array.isArray(entries) ? (entries as BoardEntry[]) : [];
	} catch {
		return [];
	}
}
