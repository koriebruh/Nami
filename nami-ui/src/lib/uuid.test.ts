import { describe, expect, it } from 'vitest';
import { params } from '../params';
import { isUuid } from './uuid';

describe('isUuid', () => {
	it('accepts the canonical hyphenated form in either case', () => {
		expect(isUuid('c13bc10a-d17f-443b-8976-c6d9b54e2798')).toBe(true);
		expect(isUuid('C13BC10A-D17F-443B-8976-C6D9B54E2798')).toBe(true);
		expect(isUuid('00000000-0000-0000-0000-000000000000')).toBe(true);
	});

	it('rejects everything else', () => {
		const rejected = [
			'',
			'not-a-uuid',
			'..',
			'../auth/me?',
			'..%2Fauth%2Fme%3F',
			'c13bc10ad17f443b8976c6d9b54e2798', // no hyphens
			'{c13bc10a-d17f-443b-8976-c6d9b54e2798}', // braces
			'urn:uuid:c13bc10a-d17f-443b-8976-c6d9b54e2798',
			'c13bc10a-d17f-443b-8976-c6d9b54e2798\n', // a trailing newline
			' c13bc10a-d17f-443b-8976-c6d9b54e2798',
			'c13bc10a-d17f-443b-8976-c6d9b54e279', // one digit short
			'c13bc10a-d17f-443b-8976-c6d9b54e27988', // one digit long
			'g13bc10a-d17f-443b-8976-c6d9b54e2798' // not hex
		];
		for (const value of rejected) expect(isUuid(value), JSON.stringify(value)).toBe(false);
	});

	it('rejects values that are not strings', () => {
		expect(isUuid(undefined)).toBe(false);
		expect(isUuid(null)).toBe(false);
		expect(isUuid(42)).toBe(false);
	});
});

// SvelteKit decodes a route param (%2F becomes "/") before it runs the matcher
// of a `[id=uuid]` segment, and a value the matcher refuses never becomes a
// param: the URL is a 404.
describe('the uuid route param matcher', () => {
	const matches = (decoded: string) => {
		const result = params.uuid['~standard'].validate(decoded) as { issues?: unknown; value?: unknown };
		return !result.issues && result.value === decoded;
	};

	it('matches a UUID', () => {
		expect(matches('c13bc10a-d17f-443b-8976-c6d9b54e2798')).toBe(true);
	});

	it('refuses a value that would climb out of its path segment', () => {
		expect(matches('../auth/me?')).toBe(false);
		expect(matches('..')).toBe(false);
		expect(matches('not-a-uuid')).toBe(false);
	});
});
