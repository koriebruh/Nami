import { describe, expect, it } from 'vitest';
import { startLiveTail, type LiveStatus } from './live-tail';

class FakeEventSource {
	static CLOSED = 2;
	static last: FakeEventSource;
	readyState = 0;
	closed = false;
	onopen: (() => void) | null = null;
	onerror: (() => void) | null = null;
	listeners = new Map<string, (event: MessageEvent) => void>();
	constructor(public url: string) {
		FakeEventSource.last = this;
	}
	addEventListener(type: string, listener: (event: MessageEvent) => void) {
		this.listeners.set(type, listener);
	}
	close() {
		this.closed = true;
		this.readyState = FakeEventSource.CLOSED;
	}
	emit(type: string, data: unknown) {
		this.listeners.get(type)?.({ data: typeof data === 'string' ? data : JSON.stringify(data) } as MessageEvent);
	}
}

const row = (id: string) => ({
	id,
	source_id: 'c13bc10a-d17f-443b-8976-c6d9b54e2798',
	dt: '2026-10-19T08:00:00.000Z',
	level: 'info',
	message: id,
	fields: {},
	trace_id: '',
	span_id: '',
	pattern_hash: '1'
});

function start() {
	const seen: { status: LiveStatus[]; rows: string[][]; notice: (string | null)[] } = {
		status: [],
		rows: [],
		notice: []
	};
	const stop = startLiveTail('/p/logs/tail?level=error', {
		EventSourceImpl: FakeEventSource as unknown as typeof EventSource,
		cap: 2,
		onStatus: (status, notice) => {
			seen.status.push(status);
			seen.notice.push(notice);
		},
		onRows: (rows) => seen.rows.push(rows.map((r) => r.id))
	});
	return { seen, stop, source: FakeEventSource.last };
}

describe('startLiveTail', () => {
	it('connects, validates, dedupes and caps live rows', () => {
		const { seen, source } = start();
		expect(source.url).toBe('/p/logs/tail?level=error');
		expect(seen.status.at(-1)).toBe('connecting');
		source.onopen?.();
		expect(seen.status.at(-1)).toBe('live');
		source.emit('log', row('a'));
		source.emit('log', { id: 'broken' });
		source.emit('log', 'not json');
		source.emit('log', row('a'));
		source.emit('log', row('b'));
		source.emit('log', row('c'));
		expect(seen.rows.at(-1)).toEqual(['c', 'b']);
	});

	it('lets the browser reconnect after a timeout but stops for a dead session', () => {
		const first = start();
		first.source.emit('end', { reason: 'timeout' });
		expect(first.source.closed).toBe(false);
		expect(first.seen.status.at(-1)).toBe('reconnecting');
		const second = start();
		second.source.emit('end', { reason: 'session' });
		expect(second.source.closed).toBe(true);
		expect(second.seen.status.at(-1)).toBe('ended');
	});

	it('stops cleanly', () => {
		const { stop, source } = start();
		stop();
		expect(source.closed).toBe(true);
	});
});
