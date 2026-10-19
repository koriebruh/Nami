import { selectRange, type Availability, type SelectedRange } from '#lib/availability';
import { callEngine } from '#lib/server/engine';
import type { RequestEvent } from '@sveltejs/kit';

export type AvailabilityView = {
	range: SelectedRange;
	/** Null when there is no range yet, or the engine could not answer. */
	data: Availability | null;
	unavailable: boolean;
};

/**
 * The availability card's data: the range from the page's query string, and
 * one engine call for it (`enginePath` of the monitor or heartbeat plus
 * `/availability`). A failed call is `unavailable`, never a page error: the
 * page still loads without it.
 */
export async function loadAvailability(event: RequestEvent, availabilityPath: string): Promise<AvailabilityView> {
	const range = selectRange(event.url.searchParams, new Date());
	if (!range.from || !range.to) return { range, data: null, unavailable: false };
	try {
		const query = `?from=${encodeURIComponent(range.from)}&to=${encodeURIComponent(range.to)}`;
		const response = await callEngine(event, availabilityPath + query);
		if (!response.ok) return { range, data: null, unavailable: true };
		return { range, data: (await response.json()) as Availability, unavailable: false };
	} catch {
		return { range, data: null, unavailable: true };
	}
}
