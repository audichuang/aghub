import { useCallback, useEffect, useRef } from "react";

export interface UseMultiSelectOptions<T extends string> {
	/** Currently selected keys */
	selectedKeys: Set<T>;
	/** Callback when selection changes */
	onSelectionChange: (keys: Set<T>, clickedKey?: T) => void;
	/** Whether multi-select mode is enabled */
	isMultiSelectMode?: boolean;
}

export interface UseMultiSelectReturn<T extends string> {
	/** Creates a handler for a specific ordered keys list */
	createSelectionHandler: (
		orderedKeys: T[],
	) => (keys: "all" | Set<React.Key>) => void;
	/**
	 * Like `createSelectionHandler`, but for grouped lists where several
	 * ListBoxes share one global selection. The handler only owns its group's
	 * slice: selections outside `orderedKeys` are preserved, so one group's
	 * change never clobbers another group's.
	 */
	createGroupedSelectionHandler: (
		orderedKeys: T[],
	) => (keys: "all" | Set<React.Key>) => void;
}

/**
 * Hook for handling multi-select logic with shift+click and meta/ctrl+click support.
 *
 * Features:
 * - Shift+click: Select range from last clicked to current
 * - Meta/Ctrl+click: Toggle individual items
 * - Single click: Select single item (unless in multi-select mode)
 *
 * Returns a factory function `createSelectionHandler(orderedKeys)` that creates
 * selection handlers for different ordered key lists (useful when you have multiple
 * list boxes with different key ordering).
 */
export function useMultiSelect<T extends string>(
	options: UseMultiSelectOptions<T>,
): UseMultiSelectReturn<T> {
	const {
		selectedKeys,
		onSelectionChange,
		isMultiSelectMode = false,
	} = options;

	const modifiersRef = useRef({
		shift: false,
		meta: false,
	});
	const lastClickedRef = useRef<T | null>(null);

	useEffect(() => {
		const handler = (e: PointerEvent) => {
			modifiersRef.current = {
				shift: e.shiftKey,
				meta: e.metaKey || e.ctrlKey,
			};
		};
		window.addEventListener("pointerdown", handler, true);
		return () => window.removeEventListener("pointerdown", handler, true);
	}, []);

	const createSelectionHandler = useCallback(
		(orderedKeys: T[]) => (keys: "all" | Set<React.Key>) => {
			if (keys === "all") return;
			const newKeys = new Set(Array.from(keys).map(String) as T[]);
			const added = [...newKeys].find((k) => !selectedKeys.has(k));
			const removed = [...selectedKeys].find((k) => !newKeys.has(k));
			const clicked = (added ?? removed) as T | undefined;

			if (!clicked) {
				onSelectionChange(newKeys);
				return;
			}

			let finalKeys: Set<T>;

			if (modifiersRef.current.shift && lastClickedRef.current) {
				const start = orderedKeys.indexOf(lastClickedRef.current);
				const end = orderedKeys.indexOf(clicked);
				if (start !== -1 && end !== -1) {
					const [from, to] = [
						Math.min(start, end),
						Math.max(start, end),
					];
					finalKeys = new Set(orderedKeys.slice(from, to + 1));
				} else {
					finalKeys = new Set([...selectedKeys, clicked]);
				}
			} else if (!isMultiSelectMode && !modifiersRef.current.meta) {
				finalKeys = new Set([clicked]);
			} else {
				finalKeys = new Set(selectedKeys);
				if (finalKeys.has(clicked)) {
					finalKeys.delete(clicked);
				} else {
					finalKeys.add(clicked);
				}
			}

			if (!modifiersRef.current.shift) {
				lastClickedRef.current = clicked;
			}

			onSelectionChange(finalKeys, clicked);
		},
		[selectedKeys, onSelectionChange, isMultiSelectMode],
	);

	const createGroupedSelectionHandler = useCallback(
		(orderedKeys: T[]) => (keys: "all" | Set<React.Key>) => {
			if (keys === "all") return;
			const groupKeySet = new Set(orderedKeys);
			const incoming = new Set(Array.from(keys).map(String) as T[]);
			const prevInGroup = new Set(
				[...selectedKeys].filter((k) => groupKeySet.has(k)),
			);
			const added = [...incoming].find((k) => !prevInGroup.has(k));
			const removed = [...prevInGroup].find((k) => !incoming.has(k));
			const clicked = (added ?? removed) as T | undefined;

			// Selections outside this group are always preserved.
			const outside = [...selectedKeys].filter(
				(k) => !groupKeySet.has(k),
			) as T[];

			if (!clicked) {
				onSelectionChange(new Set([...outside, ...incoming]));
				return;
			}

			if (
				modifiersRef.current.shift &&
				lastClickedRef.current &&
				groupKeySet.has(lastClickedRef.current)
			) {
				const start = orderedKeys.indexOf(lastClickedRef.current);
				const end = orderedKeys.indexOf(clicked);
				const range =
					start !== -1 && end !== -1
						? orderedKeys.slice(
								Math.min(start, end),
								Math.max(start, end) + 1,
							)
						: [clicked];
				onSelectionChange(
					new Set([...outside, ...prevInGroup, ...range]),
					clicked,
				);
			} else if (!isMultiSelectMode && !modifiersRef.current.meta) {
				// Single-select: only the clicked item across all groups.
				lastClickedRef.current = clicked;
				onSelectionChange(new Set([clicked]), clicked);
				return;
			} else {
				const groupFinal = new Set(prevInGroup);
				if (groupFinal.has(clicked)) {
					groupFinal.delete(clicked);
				} else {
					groupFinal.add(clicked);
				}
				if (!modifiersRef.current.shift) {
					lastClickedRef.current = clicked;
				}
				onSelectionChange(
					new Set([...outside, ...groupFinal]),
					clicked,
				);
				return;
			}

			if (!modifiersRef.current.shift) {
				lastClickedRef.current = clicked;
			}
		},
		[selectedKeys, onSelectionChange, isMultiSelectMode],
	);

	return { createSelectionHandler, createGroupedSelectionHandler };
}
