<script lang="ts">
	import { enhance, type SubmitFunction } from '$app/forms';
	import {
		BODY_METHODS,
		DNS_RECORD_TYPES,
		HTTP_METHODS,
		IP_VERSIONS,
		IP_VERSION_TYPES,
		escapePayload,
		isRedacted,
		proxyMustBeReentered,
		secretsMustBeReentered,
		type CheckType,
		type DnsRecordType,
		type HttpMethod,
		type IpVersion,
		type Monitor,
		type MonitorGroup
	} from '#lib/uptime/uptime';

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
	function initialUrl(): string {
		return monitor?.url ?? '';
	}
	function initialVerifySsl(): boolean {
		return monitor?.verify_ssl ?? true;
	}
	function initialIpVersion(): IpVersion {
		return monitor?.ip_version ?? 'any';
	}
	function initialProxyUrl(): string {
		return monitor?.proxy_url ?? '';
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
	// Bound, not defaultValue: whether the stored secrets can be kept depends
	// on them (see `secretsReason`).
	let url = $state(initialUrl());
	let verifySsl = $state(initialVerifySsl());
	let ipVersion = $state<IpVersion>(initialIpVersion());
	let proxyUrl = $state(initialProxyUrl());
	// Edit only: what to do with the stored write-only secrets.
	let headersAction = $state<'keep' | 'replace' | 'clear'>('keep');
	let passwordAction = $state<'keep' | 'replace' | 'clear'>('keep');
	let proxyPasswordAction = $state<'keep' | 'replace' | 'clear'>('keep');

	const editing = $derived(monitor !== null);
	const isHttp = $derived(checkType === 'http' || checkType === 'keyword');
	const usesIpVersion = $derived(IP_VERSION_TYPES.includes(checkType));
	// A viewer reads `***` for URL secrets, the body, the username and the
	// udp payload (the engine flags the view `redacted`): that monitor is
	// shown as it is, read-only, because saving would write the mask over the
	// real values (the engine refuses a viewer's save anyway).
	const masked = $derived(monitor !== null && isRedacted(monitor));
	const storedHeaders = $derived((monitor?.request_headers.length ?? 0) > 0);
	const storedPassword = $derived(monitor?.has_auth_password ?? false);
	// The engine never sends stored secrets to another origin, nor over https
	// without certificate checks: an edit that does either must enter them
	// again or remove them, so "Keep" is taken away and replace is preselected.
	const secretsReason = $derived(
		editing && (storedHeaders || storedPassword)
			? secretsMustBeReentered({
					storedUrl: monitor?.url ?? null,
					url,
					storedVerifySsl: monitor?.verify_ssl ?? true,
					verifySsl
				})
			: null
	);
	const storedProxyPassword = $derived(monitor?.has_proxy_password ?? false);
	// Pointing the proxy elsewhere would let a stored secret travel to the new
	// proxy: same rule, same forced re-entry. Only http and keyword checks use
	// a proxy; the engine refuses the same edit with its own message.
	const proxyReason = $derived(
		editing && isHttp && (storedHeaders || storedPassword || storedProxyPassword)
			? proxyMustBeReentered({ storedProxyUrl: monitor?.proxy_url ?? null, proxyUrl })
			: null
	);
	const keepBlocked = $derived(secretsReason !== null || proxyReason !== null);
	$effect(() => {
		if (!keepBlocked) return;
		if (storedHeaders && headersAction === 'keep') headersAction = 'replace';
		if (storedPassword && passwordAction === 'keep') passwordAction = 'replace';
	});
	$effect(() => {
		if (proxyReason === null) return;
		if (storedProxyPassword && proxyPasswordAction === 'keep') proxyPasswordAction = 'replace';
	});
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

	const input = 'input';
	const label = 'label';
	const hint = 'hint';
	const radio = 'flex items-center gap-2 text-sm text-ink-2';
	const section = 'space-y-4 rounded-card border border-border bg-surface-2/60 p-4 sm:p-5';
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
		<div role="alert" class="alert alert-danger">
			{message}
		</div>
	{/if}
	{#if masked}
		<div class="alert border-info-ink/25 bg-info-soft text-info-ink">
			Parts of this monitor (URL secrets, request body, username, UDP payload) are hidden for your role, so it is shown
			read-only.
		</div>
	{/if}

	{#if monitor}
		<input type="hidden" name="initial_check_type" defaultValue={monitor.check_type} />
		<input type="hidden" name="initial_url" defaultValue={monitor.url ?? ''} />
		{#if monitor.verify_ssl}<input type="hidden" name="initial_verify_ssl" defaultValue="on" />{/if}
		{#if storedHeaders}<input type="hidden" name="stored_headers" defaultValue="on" />{/if}
		{#if storedPassword}<input type="hidden" name="stored_password" defaultValue="on" />{/if}
		<input type="hidden" name="initial_proxy_url" defaultValue={monitor.proxy_url ?? ''} />
		{#if storedProxyPassword}<input type="hidden" name="stored_proxy_password" defaultValue="on" />{/if}
	{/if}

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
						bind:value={url}
						class={input}
					/>
					{#if secretsReason}
						<p class="mt-1 text-xs text-warning-ink" role="status">{secretsReason}: see Custom headers and Basic auth below.</p>
					{/if}
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
				<summary class="cursor-pointer text-sm font-medium text-ink">Request options</summary>
				<p class="text-xs text-muted">
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
						<p class="text-xs text-muted">
							{#if monitor?.request_headers.length}
								Stored: {monitor.request_headers.map((h) => h.name).join(', ')}. Values are never shown.
							{:else}
								No custom headers.
							{/if}
						</p>
						<div class="flex flex-wrap gap-4">
							<label class={radio}
								><input type="radio" name="headers_action" value="keep" bind:group={headersAction} disabled={keepBlocked && storedHeaders} />
								Keep</label
							>
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
									<input
										type="radio"
										name="password_action"
										value="keep"
										bind:group={passwordAction}
										disabled={keepBlocked && storedPassword}
									/>
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
						<input type="checkbox" name="verify_ssl" bind:checked={verifySsl} />
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

		{#if usesIpVersion}
			<details
				class={section}
				open={!!(monitor && (monitor.ip_version !== 'any' || monitor.remember_cookies || monitor.proxy_url)) || undefined}
			>
				<summary class="cursor-pointer text-sm font-medium text-ink">Advanced</summary>
				<div class="sm:w-1/2">
					<label for="ip_version" class={label}>IP version</label>
					<select id="ip_version" name="ip_version" bind:value={ipVersion} class={input}>
						{#each IP_VERSIONS as option (option.value)}
							<option value={option.value}>{option.label}</option>
						{/each}
					</select>
					<p class={hint}>There is no fallback: with IPv6 only, a target without an IPv6 address goes Down.</p>
				</div>

				{#if isHttp}
					<div>
						<label class={radio}>
							<input type="checkbox" name="remember_cookies" defaultChecked={monitor?.remember_cookies ?? false} />
							Remember cookies during a check
						</label>
						<p class={hint}>Only for this check's redirects; never stored.</p>
					</div>

					<fieldset class="space-y-3">
						<legend class={label}>HTTP proxy</legend>
						<div>
							<label for="proxy_url" class={label}>Proxy URL</label>
							<input
								id="proxy_url"
								name="proxy_url"
								type="url"
								maxlength="2048"
								placeholder="http://proxy.internal:3128"
								bind:value={proxyUrl}
								class={input}
							/>
							<p class={hint}>The proxy resolves the target, so IP version must be Any.</p>
							{#if proxyUrl.trim() !== '' && ipVersion !== 'any'}
								<p class="mt-1 text-xs text-warning-ink" role="status">A proxy cannot be combined with an IP version other than Any.</p>
							{/if}
							{#if proxyReason}
								<p class="mt-1 text-xs text-warning-ink" role="status">
									{proxyReason}: see Custom headers, Basic auth and the proxy password.
								</p>
							{/if}
						</div>
						<div class="grid gap-4 sm:grid-cols-2">
							<div>
								<label for="proxy_username" class={label}>Proxy username</label>
								<input id="proxy_username" name="proxy_username" type="text" autocomplete="off" maxlength="256" defaultValue={monitor?.proxy_username ?? ''} class={input} />
							</div>
							<fieldset class="space-y-2">
								<legend class={label}>Proxy password</legend>
								{#if editing}
									<div class="flex flex-wrap gap-4">
										<label class={radio}>
											<input
												type="radio"
												name="proxy_password_action"
												value="keep"
												bind:group={proxyPasswordAction}
												disabled={proxyReason !== null && storedProxyPassword}
											/>
											Keep {monitor?.has_proxy_password ? '(set)' : '(none)'}
										</label>
										<label class={radio}><input type="radio" name="proxy_password_action" value="replace" bind:group={proxyPasswordAction} /> Replace</label>
										<label class={radio}><input type="radio" name="proxy_password_action" value="clear" bind:group={proxyPasswordAction} /> Remove</label>
									</div>
								{/if}
								{#if !editing || proxyPasswordAction === 'replace'}
									<input name="proxy_password" type="password" autocomplete="new-password" maxlength="1024" aria-label="Proxy password" class={input} />
								{/if}
							</fieldset>
						</div>
					</fieldset>
				{/if}
			</details>
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
				<p id="timeout-hint" class="text-xs text-muted sm:col-span-4">
					Timeout must be less than the interval. {editing
						? 'Empty fields keep the current value.'
						: 'Empty fields use the default shown.'}
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
				class="btn btn-primary">
				{submitLabel}
			</button>
		{/if}
		<a href={cancelHref} class="text-sm text-muted hover:text-ink-2">{masked ? 'Back' : 'Cancel'}</a>
	</div>
{/snippet}
