export interface InstalledSkillRef {
	source: string;
	name: string;
}

/**
 * The lock records a GitHub source as `owner/repo` — `aghub-git`'s
 * `resolve_remote_source` strips the `github/` prefix before the entry is
 * written — while skills.sh rows and the featured catalog spell the same
 * source `github/owner/repo`. Comparing the raw strings makes EVERY installed
 * market skill look uninstalled, so both sides are keyed on the stripped form.
 */
function lockSourceSpelling(source: string): string {
	return source.startsWith("github/")
		? source.slice("github/".length)
		: source;
}

export function installedKey(source: string, name: string): string {
	return `${lockSourceSpelling(source)}|${name}`;
}

export function buildInstalledSet(
	entries: Iterable<InstalledSkillRef>,
): Set<string> {
	const set = new Set<string>();
	for (const entry of entries) {
		set.add(installedKey(entry.source, entry.name));
	}
	return set;
}

export function isSkillInstalled(
	set: Set<string>,
	source: string,
	name: string,
): boolean {
	return set.has(installedKey(source, name));
}
