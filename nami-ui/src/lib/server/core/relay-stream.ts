/**
 * Hands `body` on chunk by chunk (back-pressure kept, nothing buffered). If
 * the upstream errors or aborts midway, the returned stream errors with it,
 * so the response ends as a broken connection: a clean close would make a
 * cut-off CSV look complete. The reader is cancelled when the client leaves.
 */
export function relayStream(body: ReadableStream<Uint8Array>): ReadableStream<Uint8Array> {
	const reader = body.getReader();
	return new ReadableStream<Uint8Array>({
		async pull(controller) {
			try {
				const { done, value } = await reader.read();
				if (done) controller.close();
				else controller.enqueue(value);
			} catch (cause) {
				controller.error(cause);
			}
		},
		cancel(reason) {
			return reader.cancel(reason);
		}
	});
}
