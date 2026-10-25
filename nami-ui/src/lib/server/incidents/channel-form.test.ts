import { describe, expect, it } from 'vitest';
import { channelPayloadFromForm } from './channel-form';

function form(fields: Record<string, string>) {
	const f = new FormData();
	for (const [k, v] of Object.entries(fields)) f.set(k, v);
	return f;
}

describe('channelPayloadFromForm', () => {
	it('builds a create payload per kind', () => {
		expect(channelPayloadFromForm(form({ name: ' Ops ', kind: 'slack', webhook_url: ' https://hooks.slack.com/services/x ' }), 'create', 'project'))
			.toEqual({ ok: true, payload: { name: 'Ops', kind: 'slack', config: { webhook_url: 'https://hooks.slack.com/services/x' }, enabled: true } });
		expect(channelPayloadFromForm(form({ name: 'tg', kind: 'telegram', bot_token: '1:abc', chat_id: '42' }), 'create', 'personal'))
			.toMatchObject({ ok: true, payload: { config: { bot_token: '1:abc', chat_id: '42' } } });
	});

	it('requires every config field on create and refuses unknown or personal email kinds', () => {
		expect(channelPayloadFromForm(form({ name: 'tg', kind: 'telegram', bot_token: '1:abc' }), 'create', 'personal')).toMatchObject({ ok: false });
		expect(channelPayloadFromForm(form({ name: 'x', kind: 'sms' }), 'create', 'project')).toMatchObject({ ok: false });
		expect(channelPayloadFromForm(form({ name: 'x', kind: 'email', address: 'a@b.test' }), 'create', 'personal')).toMatchObject({ ok: false });
	});

	it('keeps secrets on edit unless all of them are replaced', () => {
		expect(channelPayloadFromForm(form({ name: 'Ops', kind: 'slack', webhook_url: '', enabled: 'on' }), 'edit', 'project'))
			.toEqual({ ok: true, payload: { name: 'Ops', enabled: true } });
		expect(channelPayloadFromForm(form({ name: 'tg', kind: 'telegram', bot_token: '', chat_id: '43' }), 'edit', 'personal'))
			.toMatchObject({ ok: false, message: expect.stringContaining('both') });
		expect(channelPayloadFromForm(form({ name: 'Ops', kind: 'slack', webhook_url: 'https://hooks.slack.com/services/y' }), 'edit', 'project'))
			.toEqual({ ok: true, payload: { name: 'Ops', enabled: false, kind: 'slack', config: { webhook_url: 'https://hooks.slack.com/services/y' } } });
	});

	// The engine refuses unknown config keys: only the chosen kind's fields go.
	it('sends only the fields of the chosen kind', () => {
		expect(channelPayloadFromForm(form({ name: 'w', kind: 'webhook', url: 'https://x.test/h', webhook_url: '', bot_token: '', address: '' }), 'create', 'project'))
			.toEqual({ ok: true, payload: { name: 'w', kind: 'webhook', config: { url: 'https://x.test/h' }, enabled: true } });
		expect(channelPayloadFromForm(form({ name: 't', kind: 'telegram', address: '', url: '', webhook_url: '', bot_token: '1:abc', chat_id: '42' }), 'create', 'project'))
			.toEqual({ ok: true, payload: { name: 't', kind: 'telegram', config: { bot_token: '1:abc', chat_id: '42' }, enabled: true } });
	});
});
