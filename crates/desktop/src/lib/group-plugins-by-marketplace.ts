// Pure grouping helpers for marketplace-grouped plugin views.
//
// Used by the installed-list pane (grouping by the `@source` segment of a
// plugin id) and the market dialog (grouping by the explicit `marketplace`
// field). The helper is generic / type-agnostic so both surfaces share the
// same sort + official-pinned logic.

const OFFICIAL_MARKETPLACE_KEY = "claude-plugins-official";

export interface MarketplaceGroup<T> {
	/** Stable grouping key (marketplace name). */
	key: string;
	/** Human-facing header label (resolved repo label, or the key as fallback). */
	label: string;
	/** Whether the source resolves to a GitHub repo (icon hint). */
	isGithub: boolean;
	items: T[];
}

/**
 * Group items by marketplace.
 *
 * - Item order within a group is preserved (callers pass pre-sorted items).
 * - Groups are ordered: the official marketplace first, then by label
 *   (locale-aware) ascending.
 * - The header (`label`, `isGithub`) is taken from the first item seen for each
 *   key; same-marketplace items share these, so any member is representative.
 */
export function groupByMarketplace<T>(
	items: T[],
	getKey: (item: T) => string,
	getHeader: (item: T) => { label: string; isGithub: boolean },
): MarketplaceGroup<T>[] {
	const byKey = new Map<string, MarketplaceGroup<T>>();
	for (const item of items) {
		const key = getKey(item);
		let group = byKey.get(key);
		if (!group) {
			const header = getHeader(item);
			group = {
				key,
				label: header.label,
				isGithub: header.isGithub,
				items: [],
			};
			byKey.set(key, group);
		}
		group.items.push(item);
	}

	return [...byKey.values()].sort((a, b) => {
		if (a.key === OFFICIAL_MARKETPLACE_KEY) {
			return b.key === OFFICIAL_MARKETPLACE_KEY ? 0 : -1;
		}
		if (b.key === OFFICIAL_MARKETPLACE_KEY) {
			return 1;
		}
		return a.label.localeCompare(b.label);
	});
}

/**
 * Extract the marketplace name from a plugin id of the form `name@source`
 * (mirrors the backend `PluginId` `rsplit_once('@')` contract). Returns "" when
 * the id has no `@`, so such plugins land in a single "unknown" bucket instead
 * of being dropped.
 */
export function pluginMarketplaceKey(id: string): string {
	const at = id.lastIndexOf("@");
	return at === -1 ? "" : id.slice(at + 1);
}
