import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { selectRange, type Availability } from '#lib/uptime/availability';
import AvailabilityCard from './AvailabilityCard.svelte';

const NOW = new Date('2026-05-17T12:00:00Z');
const data = (over: Partial<Availability> = {}): Availability => ({
	from: '2026-04-17T12:00:00Z',
	to: '2026-05-17T12:00:00Z',
	total_seconds: 2_592_000,
	maintenance_seconds: 3_600,
	downtime_seconds: 1_500,
	uptime_pct: 99.9421,
	incidents: 2,
	longest_incident_seconds: 900,
	...over
});
const view = (query: string, d: Availability | null, unavailable = false) => ({
	range: selectRange(new URLSearchParams(query), NOW),
	data: d,
	unavailable
});
const html = (props: Record<string, unknown>) => render(AvailabilityCard, { props: props as never }).body;

describe('AvailabilityCard', () => {
	it('shows uptime, downtime, incidents, the longest incident and the maintenance left out', () => {
		const body = html({ availability: view('', data()) });
		expect(body).toContain('99.942');
		expect(body).toContain('25m');
		expect(body).toContain('15m');
		expect(body).toContain('1h maintenance excluded');
		expect(body).toContain('Longest incident');
	});

	it('says no data, never 0 %, when the uptime is null', () => {
		const body = html({ availability: view('', data({ uptime_pct: null, incidents: 0 })) });
		expect(body).toContain('No data');
		expect(body).not.toContain('0.000');
	});

	it('marks the chosen preset and links the others', () => {
		const body = html({ availability: view('range=7d', data()) });
		expect(body).toMatch(/href="\?range=7d"[^>]*aria-current="page"|aria-current="page"[^>]*>7d/);
		expect(body).toContain('href="?range=90d"');
		expect(body).toContain('href="?range=custom"');
	});

	it('offers the CSV export only with an export route, as a menu item', () => {
		const withExport = html({ availability: view('', data()), exportBase: '/projects/p/monitors/m/checks.csv' });
		expect(withExport).toContain('class="menu-item"');
		expect(withExport).toContain('href="/projects/p/monitors/m/checks.csv?from=');
		expect(html({ availability: view('', data()) })).not.toContain('Export CSV');
	});

	it('notes when the export is clipped to 180 days', () => {
		const body = html({ availability: view('range=custom&from=2025-01-01&to=2026-01-01', data()), exportBase: '/x/checks.csv' });
		expect(body).toContain('last 180 days');
	});

	it('shows the date form for a custom range, with the error and an unavailable note', () => {
		const body = html({ availability: view('range=custom&from=2026-02-01&to=2026-01-01', null) });
		expect(body).toContain('name="from"');
		expect(body).toContain('role="alert"');
		expect(html({ availability: view('', null, true) })).toContain('unavailable right now');
	});
});
