import { describe, expect, it } from 'vitest';
import {
	curlSnippet,
	dockerSnippet,
	formatBytes,
	formatQuota,
	ingestUrl,
	MIB,
	quotaInMib,
	rsyslogSnippet,
	vectorSnippet
} from './logs';

describe('formatBytes', () => {
	it('uses binary units with at most one decimal', () => {
		expect(formatBytes(0)).toBe('0 B');
		expect(formatBytes(1023)).toBe('1023 B');
		expect(formatBytes(1536)).toBe('1.5 KiB');
		expect(formatBytes(MIB)).toBe('1 MiB');
		expect(formatBytes(150 * MIB)).toBe('150 MiB');
		expect(formatBytes(3 * 1024 * MIB)).toBe('3 GiB');
	});

	it('formats quotas', () => {
		expect(formatQuota(null)).toBe('Unlimited');
		expect(formatQuota(10 * MIB)).toBe('10 MiB / day');
		expect(quotaInMib(null)).toBe('');
		expect(quotaInMib(MIB)).toBe('1');
		expect(quotaInMib(1.5 * MIB)).toBe('1.5');
	});
});

describe('snippet quoting', () => {
	it('single-quotes the URL for the shell, whatever it holds', () => {
		expect(curlSnippet("https://e.test/a'b", 'tok')).toContain("curl -X POST 'https://e.test/a'\\''b' \\");
	});
	it('quotes YAML values', () => {
		expect(vectorSnippet('https://e.test/api/v1/logs', 'tok')).toContain('uri: "https://e.test/api/v1/logs"');
	});
});

describe('setup snippets', () => {
	const url = ingestUrl('https://engine.example.com');
	const token = 'tok_123';

	it('point at the ingest endpoint with the token', () => {
		expect(url).toBe('https://engine.example.com/api/v1/logs');
		for (const snippet of [curlSnippet(url, token), vectorSnippet(url, token), dockerSnippet(url, token)]) {
			expect(snippet).toContain(url);
			expect(snippet).toContain(token);
		}
		expect(curlSnippet(url, token)).toContain("-H 'Authorization: Bearer tok_123'");
		expect(vectorSnippet(url, token)).toContain('strategy: bearer');
		expect(dockerSnippet(url, token)).toContain('type: docker_logs');
	});

	it('send RFC 5424 with the token in structured data for syslog', () => {
		const snippet = rsyslogSnippet('logs.example.com', 5140, token);
		expect(snippet).toContain('<%pri%>1 %timestamp:::date-rfc3339%');
		expect(snippet).toContain('[nami token=\\"tok_123\\"]');
		expect(snippet).toContain('target="logs.example.com" port="5140" protocol="tcp"');
	});
});
