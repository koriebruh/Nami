// Closes a native <details> menu on a click outside it and on Escape (then
// focus returns to its <summary>). Without JavaScript the <details> still
// opens and closes from its summary.
import type { Action } from 'svelte/action';

export const dismissible: Action<HTMLDetailsElement> = (node) => {
	function onpointerdown(event: PointerEvent) {
		if (node.open && !node.contains(event.target as Node)) node.open = false;
	}
	function onkeydown(event: KeyboardEvent) {
		if (event.key !== 'Escape' || !node.open) return;
		node.open = false;
		node.querySelector('summary')?.focus();
	}
	document.addEventListener('pointerdown', onpointerdown);
	document.addEventListener('keydown', onkeydown);
	return {
		destroy() {
			document.removeEventListener('pointerdown', onpointerdown);
			document.removeEventListener('keydown', onkeydown);
		}
	};
};
