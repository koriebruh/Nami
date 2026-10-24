// Ingest pipeline rules (engine `logs::ingest::pipeline`) and the editor's
// drafts of them. The client checks mirror the engine's so that the editor
// can say what is wrong before saving; regex validity is left to the engine
// (the preview shows its 400).
import { isFieldPath } from './logs';

export type RuleKind = 'drop' | 'remove_fields' | 'redact' | 'rename' | 'sample';

export const RULE_KINDS: { value: RuleKind; label: string }[] = [
	{ value: 'drop', label: 'Drop' },
	{ value: 'remove_fields', label: 'Remove fields' },
	{ value: 'redact', label: 'Redact' },
	{ value: 'rename', label: 'Rename' },
	{ value: 'sample', label: 'Sample' }
];

/**
 * `equals` is sent as a string (the engine compares it with the field's JSON
 * text, R-P2-4), except a whole number for `level`: the engine maps a number
 * there to a level (pino `50` = error), but the string `"50"` never matches.
 */
export type Match = { field?: string; equals?: string | number; message_regex?: string };
export type Rule = {
	type: RuleKind;
	match?: Match;
	paths?: string[];
	keys?: string[];
	regex?: string;
	from?: string;
	to?: string;
	n?: number;
};

export const MAX_RULES = 50;
export const MAX_REGEX_CHARS = 256;
export const MAX_REGEXES = 20;
export const MAX_TEST_EVENTS = 100;
const MAX_ITEMS = 50;
const MAX_KEY_CHARS = 64;
const MAX_EQUALS_CHARS = 1024;
const SAMPLE_MIN = 2;
const SAMPLE_MAX = 10_000;

export type RuleDraft = {
	type: RuleKind;
	matchMode: 'none' | 'field' | 'regex';
	field: string;
	equals: string;
	messageRegex: string;
	paths: string;
	keys: string;
	regex: string;
	from: string;
	to: string;
	n: string;
};

/** Only drop (required) and sample (optional) take a match. */
export function usesMatch(type: RuleKind): boolean {
	return type === 'drop' || type === 'sample';
}

export function emptyDraft(type: RuleKind): RuleDraft {
	return {
		type,
		matchMode: type === 'drop' ? 'field' : 'none',
		field: '',
		equals: '',
		messageRegex: '',
		paths: '',
		keys: '',
		regex: '',
		from: '',
		to: '',
		n: type === 'sample' ? '10' : ''
	};
}

export function draftFromRule(rule: Rule): RuleDraft {
	const draft = emptyDraft(rule.type);
	const match = rule.match;
	if (match?.message_regex !== undefined) {
		draft.matchMode = 'regex';
		draft.messageRegex = match.message_regex;
	} else if (match?.field !== undefined) {
		draft.matchMode = 'field';
		draft.field = match.field;
		// A stored rule may carry a number or a boolean (sent by the API directly).
		draft.equals = String(match.equals ?? '');
	} else {
		draft.matchMode = 'none';
	}
	draft.paths = (rule.paths ?? []).join(', ');
	draft.keys = (rule.keys ?? []).join(', ');
	draft.regex = rule.regex ?? '';
	draft.from = rule.from ?? '';
	draft.to = rule.to ?? '';
	draft.n = rule.n === undefined ? draft.n : String(rule.n);
	return draft;
}

/** Comma- or newline-separated items, trimmed, empties left out. */
function items(text: string): string[] {
	return text
		.split(/[,\n]/)
		.map((item) => item.trim())
		.filter((item) => item !== '');
}

/** A rename source or target, or a match field: `level`, `message` or a field path. */
function isTarget(path: string): boolean {
	return path === 'level' || path === 'message' || isFieldPath(path);
}

function regexError(pattern: string, what: string): string | null {
	if (pattern === '') return `${what} is empty.`;
	if ([...pattern].length > MAX_REGEX_CHARS) return `${what} must be at most ${MAX_REGEX_CHARS} characters.`;
	return null;
}

type Result = { ok: true; rule: Rule } | { ok: false; message: string };

function matchFromDraft(draft: RuleDraft): { ok: true; match?: Match } | { ok: false; message: string } {
	switch (draft.matchMode) {
		case 'none':
			if (draft.type === 'drop') return { ok: false, message: 'Drop needs a match.' };
			return { ok: true };
		case 'field': {
			const field = draft.field.trim();
			if (!isTarget(field)) {
				return { ok: false, message: 'The match field must be level, message or a field path like user.id.' };
			}
			if ([...draft.equals].length > MAX_EQUALS_CHARS) {
				return { ok: false, message: `The match value must be at most ${MAX_EQUALS_CHARS} characters.` };
			}
			const equals = field === 'level' && /^\d{1,3}$/.test(draft.equals) ? Number(draft.equals) : draft.equals;
			return { ok: true, match: { field, equals } };
		}
		case 'regex': {
			const error = regexError(draft.messageRegex, 'The message regex');
			if (error) return { ok: false, message: error };
			return { ok: true, match: { message_regex: draft.messageRegex } };
		}
	}
}

export function ruleFromDraft(draft: RuleDraft): Result {
	switch (draft.type) {
		case 'drop': {
			const match = matchFromDraft(draft);
			if (!match.ok) return match;
			return { ok: true, rule: { type: 'drop', match: match.match } };
		}
		case 'remove_fields': {
			const paths = items(draft.paths);
			if (paths.length < 1 || paths.length > MAX_ITEMS) {
				return { ok: false, message: `List 1 to ${MAX_ITEMS} field paths to remove.` };
			}
			if (paths.some((path) => path === 'level' || path === 'message')) {
				return { ok: false, message: 'Level and message cannot be removed; use rename or redact.' };
			}
			const bad = paths.find((path) => !isFieldPath(path));
			if (bad !== undefined) {
				return { ok: false, message: `"${bad}" is not a field path (1-5 dot-separated segments of A-Z a-z 0-9 _ -).` };
			}
			return { ok: true, rule: { type: 'remove_fields', paths } };
		}
		case 'redact': {
			const keys = items(draft.keys);
			if (keys.length === 0 && draft.regex === '') {
				return { ok: false, message: 'Redact needs keys or a regex.' };
			}
			if (keys.length > MAX_ITEMS) return { ok: false, message: `Redact may list at most ${MAX_ITEMS} keys.` };
			if (keys.some((key) => [...key].length > MAX_KEY_CHARS)) {
				return { ok: false, message: `A redact key must be at most ${MAX_KEY_CHARS} characters.` };
			}
			const rule: Rule = { type: 'redact' };
			if (keys.length > 0) rule.keys = keys;
			if (draft.regex !== '') {
				const error = regexError(draft.regex, 'The regex');
				if (error) return { ok: false, message: error };
				rule.regex = draft.regex;
			}
			return { ok: true, rule };
		}
		case 'rename': {
			const from = draft.from.trim();
			const to = draft.to.trim();
			if (!isTarget(from)) return { ok: false, message: 'Rename from must be level, message or a field path.' };
			if (!isTarget(to)) return { ok: false, message: 'Rename to must be level, message or a field path.' };
			return { ok: true, rule: { type: 'rename', from, to } };
		}
		case 'sample': {
			const text = draft.n.trim();
			const n = Number(text);
			if (!/^\d+$/.test(text) || n < SAMPLE_MIN || n > SAMPLE_MAX) {
				return { ok: false, message: `N must be a whole number from ${SAMPLE_MIN} to ${SAMPLE_MAX}.` };
			}
			const match = matchFromDraft(draft);
			if (!match.ok) return match;
			const rule: Rule = { type: 'sample', n };
			if (match.match) rule.match = match.match;
			return { ok: true, rule };
		}
	}
}

/** How many regular expressions a list of rules holds (the engine allows {@link MAX_REGEXES}). */
export function regexCount(rules: Rule[]): number {
	return rules.reduce(
		(count, rule) => count + (rule.regex !== undefined ? 1 : 0) + (rule.match?.message_regex !== undefined ? 1 : 0),
		0
	);
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

type Events = { ok: true; events: Record<string, unknown>[] } | { ok: false; message: string };

function checked(values: unknown[]): Events {
	if (values.length === 0) return { ok: false, message: 'Add at least one sample event.' };
	if (values.length > MAX_TEST_EVENTS) return { ok: false, message: `At most ${MAX_TEST_EVENTS} sample events.` };
	const bad = values.findIndex((value) => !isPlainObject(value));
	if (bad >= 0) return { ok: false, message: `Sample event ${bad + 1} is not a JSON object.` };
	return { ok: true, events: values as Record<string, unknown>[] };
}

/** A JSON array of objects, one JSON object, or NDJSON (one object per line). */
export function parseSampleEvents(text: string): Events {
	const trimmed = text.trim();
	if (trimmed === '') return { ok: false, message: 'Add at least one sample event.' };
	try {
		if (trimmed.startsWith('[')) {
			const parsed: unknown = JSON.parse(trimmed);
			return Array.isArray(parsed) ? checked(parsed) : { ok: false, message: 'The samples are not a JSON array.' };
		}
		try {
			return checked([JSON.parse(trimmed)]);
		} catch {
			const lines = trimmed.split('\n').filter((line) => line.trim() !== '');
			return checked(lines.map((line) => JSON.parse(line)));
		}
	} catch {
		return { ok: false, message: 'The samples are not valid JSON (an array, one object, or one object per line).' };
	}
}

function describeMatch(match: Match): string {
	if (match.message_regex !== undefined) return `message matches /${match.message_regex}/`;
	return `${match.field} = ${match.equals}`;
}

export function describeRule(rule: Rule): string {
	switch (rule.type) {
		case 'drop':
			return rule.match ? `Drop when ${describeMatch(rule.match)}` : 'Drop';
		case 'remove_fields':
			return `Remove ${(rule.paths ?? []).join(', ')}`;
		case 'redact': {
			const keys = rule.keys?.length ? `keys ${rule.keys.join(', ')}` : '';
			const regex = rule.regex !== undefined ? `/${rule.regex}/` : '';
			return `Redact ${[keys, regex].filter(Boolean).join(' and ')}`;
		}
		case 'rename':
			return `Rename ${rule.from} → ${rule.to}`;
		case 'sample':
			return `Keep 1 in ${rule.n} events${rule.match ? ` when ${describeMatch(rule.match)}` : ''}`;
	}
}

export type TestResult =
	| {
			outcome: 'kept';
			event: {
				dt: string;
				level: string;
				message: string;
				fields: Record<string, unknown>;
				trace_id: string;
				span_id: string;
				pattern: string;
			};
	  }
	| { outcome: 'dropped' }
	| { outcome: 'sampled_out' };
