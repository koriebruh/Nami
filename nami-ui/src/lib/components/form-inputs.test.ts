import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

// Review Focus: changing the check type (or the window kind) re-renders
// parts of these forms, and Svelte re-applies a plain value={...} or checked={...} on such
// re-renders, wiping what the user typed — seen live: picking "SSL
// certificate expiry" cleared the monitor name. Initial values must go
// through defaultValue/defaultChecked, or a binding.
describe.each(['MonitorForm.svelte', 'MaintenanceWindowForm.svelte'])('%s', (file) => {
	const source = readFileSync(new URL(`./${file}`, import.meta.url), 'utf8');

	it('never sets value={...} or checked={...} on an input or textarea', () => {
		expect(source.match(/<(input|textarea)\b[^>]*\s(value|checked)=\{[^>]*>/g) ?? []).toEqual([]);
	});

	it('gives textareas their initial text through defaultValue', () => {
		expect(source).not.toMatch(/<textarea[^>]*>[^<]+<\/textarea>/);
	});
});
