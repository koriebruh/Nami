// Shapes of nami-engine's log JSON, plus pure helpers for the log pages.
// All times render in UTC (as on the uptime pages).
import { shellQuote } from './uptime';

export type Platform = 'http' | 'syslog' | 'vector' | 'docker' | 'other';

export const PLATFORMS: Platform[] = ['http', 'syslog', 'vector', 'docker', 'other'];

export const PLATFORM_LABELS: Record<Platform, string> = {
	http: 'HTTP',
	syslog: 'Syslog',
	vector: 'Vector',
	docker: 'Docker',
	other: 'Other'
};

export type Source = {
	id: string;
	project_id: string;
	name: string;
	platform: Platform;
	retention_days: number;
	daily_byte_quota: number | null;
	created_at: string;
	updated_at: string;
};

/** Create and rotate return the token once more (members and admins only). */
export type SourceWithToken = Source & { token: string };

export type DailyUsage = {
	day: string;
	events: number;
	bytes: number;
	events_dropped: number;
	events_sampled_out: number;
	requests_rate_limited: number;
	requests_over_quota: number;
};

export const MIB = 1024 * 1024;
/** Shown in snippets until a member reveals the real token. */
export const TOKEN_PLACEHOLDER = 'YOUR_SOURCE_TOKEN';

/** "512 B", "1.5 KiB", "120 MiB". */
export function formatBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	const units = ['KiB', 'MiB', 'GiB', 'TiB'];
	let value = bytes / 1024;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${value >= 100 ? Math.round(value) : Number(value.toFixed(1))} ${units[unit]}`;
}

/** A stored quota as the MiB value the form shows ('' = unlimited). */
export function quotaInMib(quota: number | null): string {
	return quota === null ? '' : String(Number((quota / MIB).toFixed(2)));
}

/** "Unlimited", or the quota per day in readable units. */
export function formatQuota(quota: number | null): string {
	return quota === null ? 'Unlimited' : `${formatBytes(quota)} / day`;
}

export function ingestUrl(engineUrl: string): string {
	return `${engineUrl}/api/v1/logs`;
}

export function curlSnippet(url: string, token: string): string {
	return [
		`curl -X POST ${shellQuote(url)} \\`,
		`  -H ${shellQuote(`Authorization: Bearer ${token}`)} \\`,
		"  -H 'Content-Type: application/json' \\",
		`  -d '{"message":"Hello from Nami","level":"info"}'`
	].join('\n');
}

function vectorSink(url: string, token: string, input: string): string[] {
	return [
		'sinks:',
		'  nami:',
		'    type: http',
		`    inputs: [${input}]`,
		`    uri: ${JSON.stringify(url)}`,
		'    compression: gzip',
		'    encoding:',
		'      codec: json',
		'    auth:',
		'      strategy: bearer',
		`      token: ${JSON.stringify(token)}`
	];
}

export function vectorSnippet(url: string, token: string): string {
	return [
		'# vector.yaml',
		'sources:',
		'  app_logs:',
		'    type: file',
		'    include: ["/var/log/app/*.log"]',
		'',
		...vectorSink(url, token, 'app_logs')
	].join('\n');
}

/** rsyslog forwarding RFC 5424 over TCP, the token in structured data. */
export function rsyslogSnippet(host: string, port: number, token: string): string {
	return [
		'# /etc/rsyslog.d/90-nami.conf',
		'template(name="NamiFormat" type="string"',
		`  string="<%pri%>1 %timestamp:::date-rfc3339% %HOSTNAME% %app-name% %procid% %msgid% [nami token=\\"${token}\\"] %msg%\\n")`,
		`*.* action(type="omfwd" target="${host}" port="${port}" protocol="tcp" template="NamiFormat")`
	].join('\n');
}

/** Every container's logs, shipped by a Vector container. */
export function dockerSnippet(url: string, token: string): string {
	return [
		'# docker-compose.yml',
		'services:',
		'  vector:',
		'    image: timberio/vector:latest-alpine',
		'    restart: unless-stopped',
		'    volumes:',
		'      - /var/run/docker.sock:/var/run/docker.sock:ro',
		'      - ./vector.yaml:/etc/vector/vector.yaml:ro',
		'',
		'# vector.yaml',
		'sources:',
		'  docker:',
		'    type: docker_logs',
		'',
		...vectorSink(url, token, 'docker')
	].join('\n');
}
