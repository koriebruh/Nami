<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import {
		BODY_METHODS,
		DNS_RECORD_TYPES,
		HTTP_METHODS,
		escapePayload,
		hasMaskedValues,
		type CheckType,
		type DnsRecordType,
		type HttpMethod,
		type Monitor,
		type MonitorGroup
	} from '#lib/uptime';

	let {
		monitor = null,
		groups = [],
		submitLabel,
		message = null,
		cancelHref
	}: {
		monitor?: Monitor | null;
		groups?: MonitorGroup[];
		submitLabel: string;
		message?: string | null;
		cancelHref: string;
	} = $props();

	// Read once, in functions, on purpose: after mount the selects drive
	// which fields render, independent of later prop updates.
	function initialCheckType(): CheckType {
		return monitor?.check_type ?? 'http';
	}
	function initialMethod(): HttpMethod {
		return monitor?.http_method ?? 'GET';
	}
	function initialUseTls(): boolean {
		return monitor?.use_tls ?? false;
	}
	function initialKeywordMode(): 'contains' | 'not_contains' {
		return monitor?.keyword_mode ?? 'contains';
	}
	function initialRecordType(): DnsRecordType {
		return monitor?.dns_record_type ?? 'A';
	}
	function initialGroup(): string {
		return monitor?.group_id ?? '';
	}
	// Header values are write-only: the textarea of a replacement starts with
	// one "Name: " line per stored header, so that every value is asked for.
	function storedHeaderLines(): string {
		return (monitor?.request_headers ?? []).map((h) => `${h.name}: `).join('\n');
	}
	let checkType = $state<CheckType>(initialCheckType());
	let method = $state<HttpMethod>(initialMethod());
	let useTls = $state(initialUseTls());
	let keywordMode = $state(initialKeywordMode());
	let recordType = $state<DnsRecordType>(initialRecordType());
	let groupId = $state(initialGroup());
	// Edit only: what to do with the stored write-only secrets.
	let headersAction = $state<'keep' | 'replace' | 'clear'>('keep');
	let passwordAction = $state<'keep' | 'replace' | 'clear'>('keep');

	const editing = $derived(monitor !== null);
	// A viewer reads `***` for URL secrets, the body and the username: that
	// monitor is shown as it is, read-only, because saving would write the
	// mask over the real values (the engine refuses a viewer's save anyway).
	const masked = $derived(monitor !== null && hasMaskedValues(monitor));
	const isHttp = $derived(checkType === 'http' || checkType === 'keyword');
	const isMail = $derived(checkType === 'smtp' || checkType === 'pop3' || checkType === 'imap');
	const isExpiry = $derived(checkType === 'ssl_expiry' || checkType === 'domain_expiry');
	const usesPort = $derived(['tcp', 'udp', 'ssl_expiry', 'smtp', 'pop3', 'imap'].includes(checkType));
	// Keyword checks search the body, which HEAD responses do not have.
	const methods = $derived(checkType === 'keyword' ? HTTP_METHODS.filter((m) => m !== 'HEAD') : HTTP_METHODS);
	const portPlaceholder = $derived(
		checkType === 'ssl_expiry'
			? '443'
			: checkType === 'smtp'
				? useTls
					? '465'
					: '25'
				: checkType === 'pop3'
					? useTls
						? '995'
						: '110'
					: checkType === 'imap'
						? useTls
							? '993'
							: '143'
						: ''
	);
	const hostPlaceholder = $derived(
		checkType === 'dns' ? '_dmarc.example.com' : checkType === 'domain_expiry' ? 'example.com' : 'db.internal'
	);
	// Expiry checks run hourly at most (default daily) and alert on the first
	// failure, so their interval and confirmation defaults differ. A stored
	// value is only offered back while the form is on a type of the same kind:
	// switching kind starts from that kind's defaults.
	const sameKind = $derived(monitor !== null && (monitor.check_type === 'ssl_expiry' || monitor.check_type === 'domain_expiry') === isExpiry);

	$effect(() => {
		if (checkType === 'keyword' && method === 'HEAD') method = 'GET';
	});

	// The submit button stays disabled from the submit until the result is
	// applied, so a double click cannot send the form twice (two monitors).
	// The button is in the `actions` snippet below on purpose: Svelte runs all
	// of a template's dynamic attributes in one effect, and a re-run of that
	// effect must not touch the inputs. As a snippet, the button (and
	// `submitLabel`, `cancelHref`) re-render on their own.
	let submitting = $state(false);
	const submit: SubmitFunction = () => {
		submitting = true;
		return async ({ update }) => {
			try {
				await update();
			} finally {
				submitting = false;
			}
		};
	};

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none disabled:opacity-60';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
	const hint = 'mt-1 text-xs text-neutral-500';
	const radio = 'flex items-center gap-2 text-sm text-neutral-300';
	const section = 'space-y-4 rounded-lg border border-[#262626] p-4';
</script>

<!-- use:enhance keeps the typed values and the selected type on a
     validation error (the action returns fail(), so the form is not reset).
     Inputs take their initial value through defaultValue/defaultChecked:
     a plain value={...} is re-applied whenever its block re-renders (e.g.
     when the check type changes), wiping what the user typed. Selects that
     must survive such re-renders are bound to state instead. The maxlength
     values mirror the engine's size caps (url 2048 bytes, host 253
     characters, keyword and expected value 1024 bytes, header value 1024
     bytes, body 64 KiB, UDP payload 1024 bytes). The engine stays the
     authority: it counts bytes, the browser counts UTF-16 units. -->
<form method="POST" use:enhance={submit} class="space-y-5">
	{#if message}
		<div role="alert" class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
			{message}
		</div>
	{/if}
	{#if masked}
		<div class="rounded-md border border-sky-500/30 bg-sky-500/10 px-3 py-2 text-sm text-sky-300">
			Parts of this monitor (URL secrets, request body, username) are hidden for your role, so it is shown read-only.
		</div>
	{/if}

	{#if monitor}<input type="hidden" name="initial_check_type" defaultValue={monitor.check_type} />{/if}

	<fieldset disabled={masked} class="min-w-0 space-y-5">
		<div class="grid gap-4 sm:grid-cols-2">
			<div>
				<label for="name" class={label}>Name</label>
				<input id="name" name="name" type="text" required maxlength="200" defaultValue={monitor?.name ?? ''} class={input} />
			</div>
			<div>
				<label for="check_type" class={label}>Check type</label>
				<select id="check_type" name="check_type" bind:value={checkType} class={input}>
					<optgroup label="Websites and APIs">
						<option value="http">HTTP status</option>
						<option value="keyword">Keyword on page</option>
					</optgroup>
					<optgroup label="Network">
						<option value="ping">Ping (ICMP)</option>
						<option value="tcp">TCP port</option>
						<option value="udp">UDP port</option>
						<option value="dns">DNS record</option>
					</optgroup>
					<optgroup label="Email servers">
						<option value="smtp">SMTP</option>
						<option value="pop3">POP3</option>
						<option value="imap">IMAP</option>
					</optgroup>
					<optgroup label="Expiry">
						<option value="ssl_expiry">SSL certificate expiry</option>
						<option value="domain_expiry">Domain expiry</option>
					</optgroup>
				</select>
			</div>
		</div>

		{#if isHttp}
			<div class="grid gap-4 sm:grid-cols-[8rem_1fr]">
				<div>
					<label for="http_method" class={label}>Method</label>
					<select id="http_method" name="http_method" bind:value={method} class={input}>
						{#each methods as option (option)}
							<option value={option}>{option}</option>
						{/each}
					</select>
				</div>
				<div>
					<label for="url" class={label}>URL</label>
					<input
						id="url"
						name="url"
						type="url"
						required
						maxlength="2048"
						placeholder="https://example.com/health"
						defaultValue={monitor?.url ?? ''}
						class={input}
					/>
				</div>
			</div>
			{#if checkType === 'keyword'}
				<div class="grid gap-4 sm:grid-cols-2">
					<div>
						<label for="keyword" class={label}>Keyword (case-sensitive)</label>
						<input id="keyword" name="keyword" type="text" required maxlength="1024" defaultValue={monitor?.keyword ?? ''} class={input} />
						<p class={hint}>Searches the response body, so HEAD is not offered.</p>
					</div>
					<div>
						<label for="keyword_mode" class={label}>Up when the page</label>
						<select id="keyword_mode" name="keyword_mode" bind:value={keywordMode} class={input}>
							<option value="contains">contains the keyword</option>
							<option value="not_contains">does not contain the keyword</option>
						</select>
					</div>
				</div>
			{/if}
			<div class="grid gap-4 sm:grid-cols-2">
				<div>
					<label for="expected_status_min" class={label}>Lowest OK status</label>
					<input id="expected_status_min" name="expected_status_min" type="number" min="100" max="599" defaultValue={monitor?.expected_status_min ?? 200} class={input} />
				</div>
				<div>
					<label for="expected_status_max" class={label}>Highest OK status</label>
					<input id="expected_status_max" name="expected_status_max" type="number" min="100" max="599" defaultValue={monitor?.expected_status_max ?? 399} class={input} />
				</div>
			</div>

			<details
				class={section}
				open={!!(monitor && (monitor.request_headers.length || monitor.auth_username || monitor.request_body)) || undefined}
			>
				<summary class="cursor-pointer text-sm font-medium text-neutral-200">Request options</summary>
				<p class="text-xs text-neutral-500">
					With custom headers, basic auth or URL credentials, a redirect to another origin (another host or port, or
					http to https) is not followed and the monitor goes Down: point the monitor at the final URL. Basic auth and
					URL credentials cannot be combined.
				</p>
				{#if BODY_METHODS.includes(method)}
					<div>
						<label for="request_body" class={label}>Request body</label>
						<textarea
							id="request_body"
							name="request_body"
							rows="4"
							maxlength="65536"
							placeholder={'{"ping": true}'}
							defaultValue={monitor?.request_body ?? ''}
							class="{input} font-mono"
						></textarea>
						<p class={hint}>Sent as is, up to 64 KiB. Set a Content-Type header below if the server needs one.</p>
					</div>
				{/if}

				<fieldset class="space-y-2">
					<legend class={label}>Custom headers</legend>
					{#if editing}
						<p class="text-xs text-neutral-500">
							{#if monitor?.request_headers.length}
								Stored: {monitor.request_headers.map((h) => h.name).join(', ')}. Values are never shown.
							{:else}
								No custom headers.
							{/if}
						</p>
						<div class="flex flex-wrap gap-4">
							<label class={radio}><input type="radio" name="headers_action" value="keep" bind:group={headersAction} /> Keep</label>
							<label class={radio}><input type="radio" name="headers_action" value="replace" bind:group={headersAction} /> Replace all</label>
							<label class={radio}><input type="radio" name="headers_action" value="clear" bind:group={headersAction} /> Remove all</label>
						</div>
					{/if}
					{#if !editing || headersAction === 'replace'}
						<textarea
							name="request_headers"
							rows="3"
							placeholder="X-Api-Key: your-key"
							aria-label="Custom headers, one Name: value per line"
							defaultValue={storedHeaderLines()}
							class="{input} font-mono"
						></textarea>
						<p class={hint}>
							One <code>Name: value</code> per line, up to 20, each value up to 1024 bytes. Values are encrypted and
							write-only{#if editing}: replacing sets the whole list, so enter every value again, also for the headers you
								keep{/if}.
						</p>
					{/if}
				</fieldset>

				<div class="grid gap-4 sm:grid-cols-2">
					<div>
						<label for="auth_username" class={label}>Basic auth username</label>
						<input id="auth_username" name="auth_username" type="text" autocomplete="off" maxlength="256" defaultValue={monitor?.auth_username ?? ''} class={input} />
					</div>
					<fieldset class="space-y-2">
						<legend class={label}>Basic auth password</legend>
						{#if editing}
							<div class="flex flex-wrap gap-4">
								<label class={radio}>
									<input type="radio" name="password_action" value="keep" bind:group={passwordAction} />
									Keep {monitor?.has_auth_password ? '(set)' : '(none)'}
								</label>
								<label class={radio}><input type="radio" name="password_action" value="replace" bind:group={passwordAction} /> Replace</label>
								<label class={radio}><input type="radio" name="password_action" value="clear" bind:group={passwordAction} /> Remove</label>
							</div>
						{/if}
						{#if !editing || passwordAction === 'replace'}
							<input name="auth_password" type="password" autocomplete="new-password" maxlength="1024" aria-label="Basic auth password" class={input} />
						{/if}
					</fieldset>
				</div>

				<div class="flex flex-wrap gap-6">
					<label class={radio}>
						<input type="checkbox" name="follow_redirects" defaultChecked={monitor?.follow_redirects ?? true} />
						Follow redirects (up to 10)
					</label>
					<label class={radio}>
						<input type="checkbox" name="verify_ssl" defaultChecked={monitor?.verify_ssl ?? true} />
						Verify SSL certificate
					</label>
				</div>
			</details>
		{:else}
			<div class="grid gap-4 {usesPort ? 'sm:grid-cols-[1fr_8rem]' : ''}">
				<div>
					<label for="host" class={label}>{checkType === 'domain_expiry' ? 'Domain' : 'Host'}</label>
					<input id="host" name="host" type="text" required maxlength="253" placeholder={hostPlaceholder} defaultValue={monitor?.host ?? ''} class={input} />
				</div>
				{#if usesPort}
					<div>
						<label for="port" class={label}>Port</label>
						<input
							id="port"
							name="port"
							type="number"
							min="1"
							max="65535"
							required={checkType === 'tcp' || checkType === 'udp'}
							placeholder={portPlaceholder}
							defaultValue={monitor?.port ?? ''}
							class={input}
						/>
					</div>
				{/if}
			</div>

			{#if checkType === 'dns'}
				<div class="grid gap-4 sm:grid-cols-[8rem_1fr_1fr]">
					<div>
						<label for="dns_record_type" class={label}>Record</label>
						<select id="dns_record_type" name="dns_record_type" bind:value={recordType} class={input}>
							{#each DNS_RECORD_TYPES as type (type)}
								<option value={type}>{type}</option>
							{/each}
						</select>
					</div>
					<div>
						<label for="dns_nameserver" class={label}>Nameserver (optional)</label>
						<input id="dns_nameserver" name="dns_nameserver" type="text" maxlength="270" placeholder="1.1.1.1 or 1.1.1.1:53" defaultValue={monitor?.dns_nameserver ?? ''} class={input} />
					</div>
					<div>
						<label for="expected_value" class={label}>An answer must contain (optional)</label>
						<input id="expected_value" name="expected_value" type="text" maxlength="1024" defaultValue={monitor?.expected_value ?? ''} class={input} />
					</div>
				</div>
			{:else if isMail}
				<label class={radio}>
					<input type="checkbox" name="use_tls" bind:checked={useTls} />
					Implicit TLS (SMTPS / POP3S / IMAPS)
				</label>
			{:else if checkType === 'udp'}
				<div>
					<label for="udp_payload" class={label}>Payload to send</label>
					<input
						id="udp_payload"
						name="udp_payload"
						type="text"
						required
						maxlength="1024"
						placeholder={'PING\\r\\n'}
						defaultValue={escapePayload(monitor?.udp_payload ?? '')}
						class="{input} font-mono"
					/>
					<p class={hint}>Text; write \n, \r and \t for line breaks and tabs, \\ for a backslash.</p>
				</div>
				<div class="grid gap-4 sm:grid-cols-2">
					<div>
						<label for="keyword" class={label}>Reply keyword (optional)</label>
						<input id="keyword" name="keyword" type="text" maxlength="1024" defaultValue={monitor?.keyword ?? ''} class={input} />
					</div>
					<div>
						<label for="keyword_mode" class={label}>Up when the reply</label>
						<select id="keyword_mode" name="keyword_mode" bind:value={keywordMode} class={input}>
							<option value="contains">contains the keyword</option>
							<option value="not_contains">does not contain the keyword</option>
						</select>
					</div>
				</div>
			{:else if isExpiry}
				<!-- Keyed: switching between SSL and domain expiry resets the
				     threshold to the new type's default (14 / 30 days). -->
				{#key checkType}
					<div class="sm:w-1/2">
						<label for="expiry_threshold_days" class={label}>Alert when fewer days remain than</label>
						<input
							id="expiry_threshold_days"
							name="expiry_threshold_days"
							type="number"
							min="1"
							max="365"
							placeholder={checkType === 'ssl_expiry' ? '14' : '30'}
							defaultValue={monitor?.check_type === checkType ? (monitor.expiry_threshold_days ?? '') : ''}
							class={input}
						/>
						<p class={hint}>Leave empty for {checkType === 'ssl_expiry' ? '14' : '30'} days.</p>
					</div>
				{/key}
			{/if}
		{/if}

		<!-- Interval and confirmation are keyed on the kind of check: expiry
		     types have their own defaults, applied when the field is left
		     empty. The other fields keep what was typed. -->
		<div class="grid gap-4 sm:grid-cols-4">
			{#key isExpiry}
				<div>
					<label for="interval_seconds" class={label}>Check every (seconds)</label>
					<input
						id="interval_seconds"
						name="interval_seconds"
						type="number"
						min={isExpiry ? 3600 : 30}
						placeholder={isExpiry ? '86400' : '60'}
						defaultValue={sameKind ? (monitor?.interval_seconds ?? '') : ''}
						class={input}
					/>
				</div>
			{/key}
				<div>
					<label for="timeout_seconds" class={label}>Timeout (seconds)</label>
					<input id="timeout_seconds" name="timeout_seconds" type="number" min="1" max="60" aria-describedby="timeout-hint" defaultValue={monitor?.timeout_seconds ?? 10} class={input} />
				</div>
			{#key isExpiry}
				<div>
					<label for="confirmation_threshold" class={label}>Failures before alerting</label>
					<input
						id="confirmation_threshold"
						name="confirmation_threshold"
						type="number"
						min="1"
						max="10"
						placeholder={isExpiry ? '1' : '2'}
						defaultValue={sameKind ? (monitor?.confirmation_threshold ?? '') : ''}
						class={input}
					/>
				</div>
			{/key}
				<div>
					<label for="recovery_threshold" class={label}>Successes to recover</label>
					<input id="recovery_threshold" name="recovery_threshold" type="number" min="1" max="10" defaultValue={monitor?.recovery_threshold ?? 1} class={input} />
				</div>
				<!-- The engine rejects a timeout that is not shorter than the interval. -->
				<p id="timeout-hint" class="text-xs text-neutral-500 sm:col-span-4">
					Timeout must be less than the interval. Empty fields use the default shown.
				</p>
		</div>

		<div class="sm:w-1/2">
			<label for="group_id" class={label}>Group</label>
			<select id="group_id" name="group_id" bind:value={groupId} class={input}>
				<option value="">No group</option>
				{#each groups as group (group.id)}
					<option value={group.id}>{group.name}</option>
				{/each}
			</select>
		</div>
	</fieldset>

	{@render actions()}
</form>

{#snippet actions()}
	<div class="flex items-center gap-3">
		{#if !masked}
			<button
				type="submit"
				disabled={submitting}
				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
			>
				{submitLabel}
			</button>
		{/if}
		<a href={cancelHref} class="text-sm text-neutral-500 hover:text-neutral-300">{masked ? 'Back' : 'Cancel'}</a>
	</div>
{/snippet}
