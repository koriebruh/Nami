// The engine's ids (projects, monitors, users) are UUIDs.
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** True for a UUID in its canonical hyphenated form, in either case. */
export function isUuid(value: unknown): value is string {
	return typeof value === 'string' && UUID.test(value);
}
