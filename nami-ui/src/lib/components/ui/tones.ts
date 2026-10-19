// Chart and status tones mapped to the design tokens (src/routes/layout.css).
// Charts use CSS variables so SVG strokes and fills follow the theme.

export type Tone = 'primary' | 'cyan' | 'periwinkle' | 'success' | 'danger' | 'warning' | 'info' | 'neutral';

export const TONE_VAR: Record<Tone, string> = {
	primary: 'var(--color-primary)',
	cyan: 'var(--color-accent-cyan)',
	periwinkle: 'var(--color-accent-periwinkle)',
	success: 'var(--color-success)',
	danger: 'var(--color-danger)',
	warning: 'var(--color-warning)',
	info: 'var(--color-info)',
	neutral: 'var(--color-status-paused)'
};

/** AA text colour classes for a tone (a delta, a value). */
export const TONE_TEXT: Record<Tone, string> = {
	primary: 'text-primary',
	cyan: 'text-info-ink',
	periwinkle: 'text-primary',
	success: 'text-success-ink',
	danger: 'text-danger-ink',
	warning: 'text-warning-ink',
	info: 'text-info-ink',
	neutral: 'text-muted'
};

/** Ordered categorical palette for multi-series charts (tokens --color-series-1..6). */
export const SERIES_COLORS = [1, 2, 3, 4, 5, 6].map((n) => `var(--color-series-${n})`);

/** The colour of series `i` (0-based); wraps after the sixth. */
export function seriesColor(i: number): string {
	const n = SERIES_COLORS.length;
	return SERIES_COLORS[((Math.trunc(i) % n) + n) % n];
}

/** Finite numbers in a series (nulls and NaN are gaps). */
export function finitePoints(values: readonly (number | null | undefined)[]): number {
	return values.filter((v) => typeof v === 'number' && Number.isFinite(v)).length;
}
