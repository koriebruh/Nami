// Notification channels as the engine returns them: never a secret, only the
// kind and a non-secret summary (host, chat id or address).

export const CHANNEL_KINDS = ['email', 'webhook', 'slack', 'teams', 'telegram', 'discord'] as const;
export type ChannelKind = (typeof CHANNEL_KINDS)[number];

export type Channel = {
	id: string;
	project_id: string | null;
	user_id: string | null;
	name: string;
	kind: string;
	summary: string;
	enabled: boolean;
	created_at: string;
	updated_at: string;
};

export const CHANNEL_LABELS: Record<ChannelKind, string> = {
	email: 'Email',
	webhook: 'Webhook',
	slack: 'Slack',
	teams: 'Microsoft Teams',
	telegram: 'Telegram',
	discord: 'Discord'
};

export type ChannelField = { name: string; label: string; secret: boolean; placeholder: string };

/** The config fields of each kind. Every one is write-only: the engine never
 *  returns them, so an edit form starts blank and blank keeps the stored value. */
export const CHANNEL_FIELDS: Record<ChannelKind, ChannelField[]> = {
	email: [{ name: 'address', label: 'Address', secret: false, placeholder: 'oncall@example.com' }],
	webhook: [{ name: 'url', label: 'URL', secret: true, placeholder: 'https://receiver.example.com/nami' }],
	slack: [{ name: 'webhook_url', label: 'Webhook URL', secret: true, placeholder: 'https://hooks.slack.com/services/…' }],
	teams: [{ name: 'webhook_url', label: 'Workflow URL', secret: true, placeholder: 'https://…' }],
	telegram: [
		{ name: 'bot_token', label: 'Bot token', secret: true, placeholder: '123456:ABC…' },
		{ name: 'chat_id', label: 'Chat ID', secret: false, placeholder: '-1001234567890 or @channel' }
	],
	discord: [{ name: 'webhook_url', label: 'Webhook URL', secret: true, placeholder: 'https://discord.com/api/webhooks/…' }]
};

export function isChannelKind(value: unknown): value is ChannelKind {
	return (CHANNEL_KINDS as readonly unknown[]).includes(value);
}

/** Personal channels have no email kind: incident email goes to the account address. */
export function kindsFor(scope: 'personal' | 'project'): readonly ChannelKind[] {
	return scope === 'personal' ? CHANNEL_KINDS.filter((k) => k !== 'email') : CHANNEL_KINDS;
}
