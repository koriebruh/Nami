import { describe, expect, it } from 'vitest';
import { renderMarkdown } from './markdown';

describe('renderMarkdown', () => {
	it('renders the supported subset', () => {
		expect(renderMarkdown('# Cause\n\nThe **db** was *slow*, see `pg_stat`.\n\n- one\n- two'))
			.toBe('<h1>Cause</h1><p>The <strong>db</strong> was <em>slow</em>, see <code>pg_stat</code>.</p><ul><li>one</li><li>two</li></ul>');
		expect(renderMarkdown('```\nselect 1;\n```')).toBe('<pre><code>select 1;</code></pre>');
		expect(renderMarkdown('[runbook](https://wiki.test/a?b=1&c=2)'))
			.toBe('<p><a href="https://wiki.test/a?b=1&amp;c=2" rel="noopener noreferrer nofollow">runbook</a></p>');
	});

	it.each([
		['<script>alert(1)</script>', '<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>'],
		['<img src=x onerror=alert(1)>', '<p>&lt;img src=x onerror=alert(1)&gt;</p>'],
		['[x](javascript:alert(1))', '<p>[x](javascript:alert(1))</p>'],
		['[x](data:text/html,<b>)', '<p>[x](data:text/html,&lt;b&gt;)</p>'],
		['[x](https://a.test/"onmouseover="alert(1))', '<p><a href="https://a.test/&quot;onmouseover=&quot;alert(1)" rel="noopener noreferrer nofollow">x</a></p>'],
		['**<b>bold</b>**', '<p><strong>&lt;b&gt;bold&lt;/b&gt;</strong></p>'],
		['```\n<script>x</script>\n```', '<pre><code>&lt;script&gt;x&lt;/script&gt;</code></pre>']
	])('never passes HTML or script: %s', (input, expected) => {
		expect(renderMarkdown(input)).toBe(expected);
	});

	it('copes with huge and unbalanced input', () => {
		expect(renderMarkdown('*'.repeat(100_000)).length).toBeGreaterThan(0);
		expect(renderMarkdown('**open')).toBe('<p>**open</p>');
	});

	// Not in the plan's list: emphasis must never reach inside rendered code
	// or a link's href, and the rest of the subset (ordered lists, heading
	// levels, line endings, the cap).
	it('keeps emphasis out of code spans and link targets', () => {
		expect(renderMarkdown('`a_b_c` and [w](https://x.test/a_b_c*d*)')).toBe(
			'<p><code>a_b_c</code> and <a href="https://x.test/a_b_c*d*" rel="noopener noreferrer nofollow">w</a></p>'
		);
		expect(renderMarkdown('[x](https://a.test/`y`)')).toBe('<p>[x](https://a.test/<code>y</code>)</p>');
		expect(renderMarkdown('snake_case_name stays _as is_')).toBe('<p>snake_case_name stays <em>as is</em></p>');
	});

	it('renders ordered lists, heading levels and CRLF input', () => {
		expect(renderMarkdown('### Steps\r\n1. stop\r\n2. start')).toBe('<h3>Steps</h3><ol><li>stop</li><li>start</li></ol>');
		expect(renderMarkdown('####### seven')).toBe('<p>####### seven</p>');
	});

	it("escapes quotes and caps the input at 102,400 characters", () => {
		expect(renderMarkdown(`it's "x"`)).toBe('<p>it&#39;s &quot;x&quot;</p>');
		expect(renderMarkdown('a'.repeat(200_000))).toBe(`<p>${'a'.repeat(102_400)}</p>`);
	});
});
