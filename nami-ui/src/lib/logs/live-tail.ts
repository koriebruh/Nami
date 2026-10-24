// The explorer's live tail: one EventSource and the rows it delivered, apart
// from the page so the lifecycle can be tested with a fake EventSource.
import { isLogEntry, LIVE_ROW_CAP, mergeLiveRows, type LogEntry } from './logs';

export type LiveStatus = 'connecting' | 'live' | 'reconnecting' | 'ended';

export type LiveTailOptions = {
	/** The EventSource constructor (tests pass a fake). */
	EventSourceImpl?: typeof EventSource;
	/** Most rows held; older ones fall off. */
	cap?: number;
	onStatus: (status: LiveStatus, notice: string | null) => void;
	/** Every change of the held rows, newest first. */
	onRows: (rows: LogEntry[]) => void;
};

/**
 * Opens the stream at `url` and reports its status and rows until the
 * returned function is called. Rows are validated, deduplicated by id and
 * capped. Reconnects are left to EventSource (which resends Last-Event-ID,
 * so the engine replays what was missed); only a dead session closes it.
 */
export function startLiveTail(url: string, options: LiveTailOptions): () => void {
	const { EventSourceImpl = EventSource, cap = LIVE_ROW_CAP, onStatus, onRows } = options;
	let rows: LogEntry[] = [];
	let status: LiveStatus = 'connecting';
	const report = (next: LiveStatus, notice: string | null) => {
		status = next;
		onStatus(next, notice);
	};
	const stream = new EventSourceImpl(url);
	report('connecting', null);
	stream.onopen = () => report('live', null);
	stream.addEventListener('log', (event) => {
		let row: unknown;
		try {
			row = JSON.parse((event as MessageEvent).data);
		} catch {
			return;
		}
		if (!isLogEntry(row)) return;
		rows = mergeLiveRows(rows, [row], cap);
		onRows(rows);
	});
	// The engine ends a stream after 30 minutes ("timeout": EventSource
	// reconnects by itself) or when the session is no longer valid
	// ("session": no reconnect, it would only be refused again).
	stream.addEventListener('end', (event) => {
		let reason: unknown;
		try {
			reason = JSON.parse((event as MessageEvent).data).reason;
		} catch {
			reason = null;
		}
		if (reason === 'session') {
			stream.close();
			report('ended', 'Live tail stopped: your session ended or access changed. Reload the page to resume.');
		} else {
			report('reconnecting', 'The live stream reached its time limit and is reconnecting…');
		}
	});
	stream.onerror = () => {
		if (stream.readyState === EventSourceImpl.CLOSED) {
			report('ended', 'Live tail could not be started (check the filters, or sign in again). Reload to retry.');
		} else if (status === 'live') {
			report('reconnecting', null);
		}
	};
	const hide = () => {
		stream.close();
		report('ended', 'Live tail paused while the page was hidden. Reload to resume.');
	};
	if (typeof window !== 'undefined') window.addEventListener('pagehide', hide);
	return () => {
		if (typeof window !== 'undefined') window.removeEventListener('pagehide', hide);
		stream.close();
	};
}
