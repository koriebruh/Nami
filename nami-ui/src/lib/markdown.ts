// A small, safe markdown renderer (ruling R22), for postmortems and later for
// dashboard text panels. Every block's text is HTML-escaped before any inline
// rule runs, so the only markup in the output is the fixed subset below:
// headings, paragraphs, lists, fenced code, code spans, strong, emphasis and
// http(s) links. Its output is the only thing the UI passes to `{@html}`.

/** Longest input rendered, in characters; the rest is cut off. */
const MAX_INPUT_CHARS = 102_400;

const ESCAPES: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

function escapeHtml(text: string): string {
	return text.replace(/[&<>"']/g, (c) => ESCAPES[c]);
}

const FENCE = /^```/;
const HEADING = /^(#{1,6})\s+(.*)$/;
const LIST_ITEM = /^\s*([-*]|\d+\.)\s+(.*)$/;

// Inline rules, run on escaped text. Each character class excludes its own
// marker and newlines, so a run of unmatched markers fails fast (linear time
// even for 100,000 stars) and an unbalanced marker stays literal.
const CODE_SPAN = /`([^`\n]+)`/g;
// The target allows one level of balanced parentheses: `https://x/a(1)`.
const LINK = /\[([^\]\n]+)\]\(((?:[^()\s]|\([^()\s]*\))+)\)/g;
// No NUL: a held code span inside a target keeps the link literal.
const SAFE_URL = /^https?:\/\/[^\s<>\0]+$/;
const STRONG = /\*\*([^*\n]+)\*\*/g;
const EM_STAR = /\*([^*\n]+)\*/g;
// `_` only at word edges, so snake_case_names stay as they are.
const EM_UNDERSCORE = /(?<![A-Za-z0-9])_([^_\n]+)_(?![A-Za-z0-9])/g;
// Placeholder for finished inline HTML (NUL never survives the input clean-up).
const TOKEN = /\0(\d+)\0/g;

function inline(escaped: string): string {
	const done: string[] = [];
	const hold = (html: string) => `\0${done.push(html) - 1}\0`;
	const restore = (text: string) => text.replace(TOKEN, (_, i: string) => done[Number(i)]);
	// Code and links are finished first and held aside, so that emphasis never
	// reaches inside a code span or an href.
	let text = escaped.replace(CODE_SPAN, (_, code: string) => hold(`<code>${code}</code>`));
	text = text.replace(LINK, (match, label: string, url: string) =>
		SAFE_URL.test(url) ? hold(`<a href="${url}" rel="noopener noreferrer nofollow">${restore(label)}</a>`) : match
	);
	text = text
		.replace(STRONG, '<strong>$1</strong>')
		.replace(EM_STAR, '<em>$1</em>')
		.replace(EM_UNDERSCORE, '<em>$1</em>');
	return restore(text);
}

/** The HTML of `source`'s supported markdown subset; never raw HTML. */
export function renderMarkdown(source: string): string {
	const lines = source.replace(/\0/g, '').replace(/\r\n?/g, '\n').slice(0, MAX_INPUT_CHARS).split('\n');
	let html = '';
	let i = 0;
	while (i < lines.length) {
		const line = lines[i];
		if (line.trim() === '') {
			i++;
		} else if (FENCE.test(line)) {
			// An unterminated fence runs to the end, as in CommonMark.
			const code: string[] = [];
			for (i++; i < lines.length && !FENCE.test(lines[i]); i++) code.push(lines[i]);
			i++;
			html += `<pre><code>${escapeHtml(code.join('\n'))}</code></pre>`;
		} else if (HEADING.test(line)) {
			const [, hashes, text] = HEADING.exec(line)!;
			html += `<h${hashes.length}>${inline(escapeHtml(text.trim()))}</h${hashes.length}>`;
			i++;
		} else if (LIST_ITEM.test(line)) {
			const ordered = /\d/.test(LIST_ITEM.exec(line)![1]);
			const tag = ordered ? 'ol' : 'ul';
			let items = '';
			for (; i < lines.length; i++) {
				const item = LIST_ITEM.exec(lines[i]);
				if (!item || /\d/.test(item[1]) !== ordered) break;
				items += `<li>${inline(escapeHtml(item[2].trim()))}</li>`;
			}
			html += `<${tag}>${items}</${tag}>`;
		} else {
			const paragraph: string[] = [];
			for (; i < lines.length; i++) {
				const next = lines[i];
				if (next.trim() === '' || FENCE.test(next) || HEADING.test(next) || LIST_ITEM.test(next)) break;
				paragraph.push(next.trim());
			}
			html += `<p>${inline(escapeHtml(paragraph.join('\n')))}</p>`;
		}
	}
	return html;
}
