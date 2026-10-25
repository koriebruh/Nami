import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import RoutingSelects from './RoutingSelects.svelte';

const P = '7e8f9a0b-1c2d-4e3f-8a5b-6c7d8e9f0a1b';
const S = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
const policies = [{ id: P, name: 'Night', is_default: false }];
const severities = [{ id: S, name: 'critical', rank: 1, is_default: false }];

const body = (props: Record<string, unknown>) => render(RoutingSelects, { props: { policies, severities, idPrefix: 't', ...props } as never }).body;

describe('RoutingSelects', () => {
	it('selects "Project default" when nothing is stored, and the stored ids otherwise', () => {
		expect(body({})).toMatch(/<option value=""[^>]*selected[^>]*>Project default/);
		const stored = body({ policyId: P, severityId: S });
		expect(stored).toMatch(new RegExp(`<option value="${P}"[^>]*selected`));
		expect(stored).toMatch(new RegExp(`<option value="${S}"[^>]*selected`));
	});

	// The lists did not load: saving must keep the stored values, not clear them.
	it('keeps a stored id the lists do not hold selectable and selected', () => {
		const html = render(RoutingSelects, { props: { policies: [], severities: [], policyId: P, severityId: S, idPrefix: 't' } }).body;
		expect(html).toMatch(new RegExp(`<option value="${P}"[^>]*selected[^>]*>Current policy`));
		expect(html).toMatch(new RegExp(`<option value="${S}"[^>]*selected[^>]*>Current severity`));
	});
});
