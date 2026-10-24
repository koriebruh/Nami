<!-- The escalation policy and severity of a monitor, heartbeat or log alert
     ('' = the project default). A stored id the lists do not hold (they did
     not load) stays selectable, so saving never moves the source back to the
     default by accident. -->
<script lang="ts">
	import type { PolicyOption, Severity } from '#lib/incidents/incidents';

	let {
		policies,
		severities,
		policyId = null,
		severityId = null,
		idPrefix
	}: {
		policies: PolicyOption[];
		severities: Severity[];
		policyId?: string | null;
		severityId?: string | null;
		idPrefix: string;
	} = $props();

	const missingPolicy = $derived(policyId && !policies.some((p) => p.id === policyId) ? policyId : null);
	const missingSeverity = $derived(severityId && !severities.some((s) => s.id === severityId) ? severityId : null);
</script>

<div>
	<label for="{idPrefix}-policy" class="label">Escalation policy</label>
	<select id="{idPrefix}-policy" name="escalation_policy_id" class="select">
		<option value="" selected={!policyId}>Project default</option>
		{#each policies as policy (policy.id)}
			<option value={policy.id} selected={policy.id === policyId}>{policy.name}{policy.is_default ? ' (default)' : ''}</option>
		{/each}
		{#if missingPolicy}
			<option value={missingPolicy} selected>Current policy</option>
		{/if}
	</select>
</div>
<div>
	<label for="{idPrefix}-severity" class="label">Severity</label>
	<select id="{idPrefix}-severity" name="severity_id" class="select">
		<option value="" selected={!severityId}>Project default</option>
		{#each severities as severity (severity.id)}
			<option value={severity.id} selected={severity.id === severityId}>{severity.name}{severity.is_default ? ' (default)' : ''}</option>
		{/each}
		{#if missingSeverity}
			<option value={missingSeverity} selected>Current severity</option>
		{/if}
	</select>
</div>
