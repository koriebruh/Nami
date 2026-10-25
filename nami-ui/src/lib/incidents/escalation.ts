// Escalation policies as the engine returns them (`/projects/{id}/escalation-policies`),
// and the editor's state. Targets travel in forms as one string each:
// "all_members" | "user:<uuid>" | "schedule:<uuid>" | "channel:<uuid>".

export const MAX_STEPS = 20;
export const MAX_TARGETS_PER_STEP = 20;
export const MAX_REPEAT_COUNT = 10;
export const MAX_DELAY_MINUTES = 1440;

export type Target =
	| { type: 'user'; id: string }
	| { type: 'schedule'; id: string }
	| { type: 'all_members' }
	| { type: 'project_channel'; id: string };

export type StepCondition = { weekdays: number[]; from: string; to: string; timezone: string };

export type PolicyView = {
	id: string;
	project_id: string;
	name: string;
	repeat_count: number;
	fallback_policy_id: string | null;
	is_default: boolean;
	steps: {
		position: number;
		delay_minutes: number;
		condition: StepCondition | null;
		/** `label`: the user's email, the schedule's name, "<channel> (<kind>)" or "All members". */
		targets: (Target & { label: string })[];
	}[];
	updated_at: string;
};

export type TargetChoice = { value: string; label: string };

export type EditorStep = {
	delay: number;
	targets: TargetChoice[];
	condition: boolean;
	weekdays: number[];
	from: string;
	to: string;
	timezone: string;
};

export type EditorState = {
	name: string;
	repeat_count: number;
	/** '' = none. */
	fallback_policy_id: string;
	steps: EditorStep[];
};

/** A target as its form value. */
export function targetValue(target: Target): string {
	switch (target.type) {
		case 'all_members':
			return 'all_members';
		case 'project_channel':
			return `channel:${target.id}`;
		default:
			return `${target.type}:${target.id}`;
	}
}

export function newStep(): EditorStep {
	return {
		delay: 0,
		targets: [{ value: 'all_members', label: 'All members' }],
		condition: false,
		weekdays: [1, 2, 3, 4, 5],
		from: '09:00',
		to: '17:00',
		timezone: 'UTC'
	};
}

/** The editor's initial values for a stored policy. */
export function policyToFormState(policy: PolicyView): EditorState {
	return {
		name: policy.name,
		repeat_count: policy.repeat_count,
		fallback_policy_id: policy.fallback_policy_id ?? '',
		steps: policy.steps.map((step) => ({
			...newStep(),
			delay: step.delay_minutes,
			targets: step.targets.map((t) => ({ value: targetValue(t), label: t.label })),
			...(step.condition && { condition: true, ...step.condition, weekdays: [...step.condition.weekdays] })
		}))
	};
}
