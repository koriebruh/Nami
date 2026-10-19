import { describe, expect, it } from 'vitest';
import { relayStream } from './relay-stream';

/** A body that sends `chunks`, then errors (or closes when `fail` is false). */
function upstream(chunks: string[], fail: boolean) {
	const enc = new TextEncoder();
	let i = 0;
	let cancelled = false;
	const body = new ReadableStream<Uint8Array>({
		pull(controller) {
			if (i < chunks.length) controller.enqueue(enc.encode(chunks[i++]));
			else if (fail) controller.error(new Error('engine aborted'));
			else controller.close();
		},
		cancel() {
			cancelled = true;
		}
	});
	return { body, wasCancelled: () => cancelled };
}

describe('relayStream', () => {
	it('closes cleanly when the upstream ends cleanly', async () => {
		const { body } = upstream(['a', 'b'], false);
		expect(await new Response(relayStream(body)).text()).toBe('ab');
	});

	it('cancels the upstream when the client leaves', async () => {
		const { body, wasCancelled } = upstream(['a', 'b', 'c'], false);
		const reader = relayStream(body).getReader();
		await reader.read();
		await reader.cancel();
		expect(wasCancelled()).toBe(true);
	});

	it('does not read ahead of the consumer', async () => {
		let pulls = 0;
		const body = new ReadableStream<Uint8Array>(
			{
				pull(controller) {
					pulls++;
					controller.enqueue(new Uint8Array([1]));
				}
			},
			{ highWaterMark: 0 }
		);
		const reader = relayStream(body).getReader();
		await reader.read();
		await new Promise((r) => setTimeout(r, 20));
		expect(pulls).toBeLessThanOrEqual(2);
	});
});
