import type { SkillUpdateResponse } from "../generated/dto";
import type { SourceUpdateBatch } from "../hooks/use-apply-all-skill-updates";

/**
 * The lock fields this grouping needs, structurally — the GLOBAL lock
 * (`SkillLockEntryResponse`) records `sourceUrl`, the PROJECT lock
 * (`LocalSkillLockEntryResponse`) does not, and both must be groupable.
 * Project scope therefore groups on the host-blind `source`, which merges two
 * forges serving one `owner/repo`; the server refuses that batch rather than
 * fetching the wrong origin, so the failure is loud, not silent.
 */
export interface LockSourceEntry {
	name: string;
	source: string;
	sourceUrl?: string | null;
}

export interface GroupedUpdates {
	/** One batch per source, sources in first-seen order, names sorted. */
	batches: SourceUpdateBatch[];
	/**
	 * Skills the check reported as `renamed`. A rename is a different
	 * transaction (`POST /skills/accept-rename`) that installs a new name and
	 * deletes the old one — folding it into an update batch would ask the
	 * server to re-fetch a skill that no longer exists upstream.
	 */
	renamed: string[];
	/**
	 * Skills that are updatable but have no lock entry, so we cannot say which
	 * source to fetch. The lock read paths fail OPEN, so this is reachable
	 * whenever the lock could not be read — dropping these silently would send
	 * a batch with an undefined `source` and 400 the whole run.
	 */
	unresolved: string[];
}

/**
 * Split pending skill updates into one batch per source.
 *
 * Grouped on `sourceUrl`, not the host-blind `source`: two forges serving the
 * same `owner/repo` are two different origins that share one `source` string,
 * and `apply-updates` fetches the origin the lock recorded. `sourceUrl` is
 * unique per origin by construction; `source` is the fallback for entries that
 * never recorded one (local sources).
 */
export function groupUpdatesBySource(
	statuses: Iterable<SkillUpdateResponse>,
	lockEntries: readonly LockSourceEntry[],
	scope: "global" | "project",
	projectRoot: string | null,
): GroupedUpdates {
	const bySkillName = new Map<string, LockSourceEntry>();
	for (const entry of lockEntries) {
		if (!bySkillName.has(entry.name)) bySkillName.set(entry.name, entry);
	}

	const batches = new Map<string, SourceUpdateBatch>();
	const renamed: string[] = [];
	const unresolved: string[] = [];

	for (const status of statuses) {
		if (status.status === "renamed") {
			renamed.push(status.name);
			continue;
		}
		if (status.status !== "updateAvailable") continue;

		const entry = bySkillName.get(status.name);
		const source = entry?.sourceUrl || entry?.source;
		if (!source) {
			unresolved.push(status.name);
			continue;
		}

		const existing = batches.get(source);
		if (existing) {
			existing.names.push(status.name);
		} else {
			batches.set(source, {
				source,
				names: [status.name],
				scope,
				projectRoot,
			});
		}
	}

	for (const batch of batches.values()) {
		batch.names.sort((a, b) => a.localeCompare(b));
	}

	return {
		batches: [...batches.values()],
		renamed: renamed.sort((a, b) => a.localeCompare(b)),
		unresolved: unresolved.sort((a, b) => a.localeCompare(b)),
	};
}

/** Total skills the batches would update. */
export function batchedSkillCount(batches: readonly SourceUpdateBatch[]) {
	return batches.reduce((sum, batch) => sum + batch.names.length, 0);
}
