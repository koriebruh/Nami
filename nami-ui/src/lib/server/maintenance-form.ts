// Maintenance-window form -> nami-engine JSON body. One-off windows are
// entered as UTC `datetime-local` values (the UI shows every time in UTC);
// weekly windows send ISO weekdays, HH:MM times and an IANA zone, all
// validated again by the engine, whose message the page shows.
import { formText, type FormMode, type FormResult } from './monitor-form';

const LOCAL_DATETIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/;

/** A `datetime-local` value read as UTC: "2026-10-13T22:00" -> "2026-10-13T22:00:00Z". */
export function utcFromLocalInput(value: string): string | null {
	if (!LOCAL_DATETIME.test(value)) return null;
	const full = `${value.length === 16 ? `${value}:00` : value}Z`;
	const date = new Date(full);
	// A real date and time: an impossible one ("2026-02-30", "T25:00") rolls
	// over in Date, so the round trip must give the same text back.
	if (Number.isNaN(date.getTime()) || date.toISOString().slice(0, 19) !== full.slice(0, 19)) return null;
	return full;
}

export function windowPayloadFromForm(form: FormData, mode: FormMode = 'create'): FormResult {
	const kind = formText(form, 'kind');
	const payload: Record<string, unknown> = { title: formText(form, 'title'), kind };
	const monitorId = formText(form, 'monitor_id');
	if (monitorId !== '' || mode === 'edit') payload.monitor_id = monitorId || null;

	if (kind === 'once') {
		const startsAt = utcFromLocalInput(formText(form, 'starts_at'));
		const endsAt = utcFromLocalInput(formText(form, 'ends_at'));
		if (!startsAt || !endsAt) return { ok: false, message: 'Enter a start and an end date and time' };
		payload.starts_at = startsAt;
		payload.ends_at = endsAt;
	} else if (kind === 'weekly') {
		const days = form
			.getAll('days_of_week')
			.map((day) => String(day))
			.filter((day) => /^\d+$/.test(day))
			.map(Number);
		if (days.length === 0) return { ok: false, message: 'Pick at least one day' };
		payload.days_of_week = days;
		payload.start_time = formText(form, 'start_time');
		payload.end_time = formText(form, 'end_time');
		payload.timezone = formText(form, 'timezone');
	}
	// Any other kind is sent as is: the engine refuses it ("kind must be once or weekly").
	return { ok: true, payload };
}
