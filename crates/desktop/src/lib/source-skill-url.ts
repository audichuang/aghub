const TRAILING_SLASH = /\/$/;
const GIT_SUFFIX = /\.git$/;
const GITHUB_REPOSITORY = /^\/[^/]+\/[^/]+$/;

/** Link to the skill document, never a clone URL or a credentials-bearing URL. */
export function sourceSkillUrl(
	source: string | undefined,
	skillPath: string,
	gitRef?: string,
): string | null {
	if (!source) return null;
	try {
		const url = new URL(source);
		if (
			url.protocol !== "https:" ||
			url.hostname !== "github.com" ||
			url.username ||
			url.password ||
			url.port ||
			url.search ||
			url.hash
		)
			return null;
		const repo = url.pathname
			.replace(TRAILING_SLASH, "")
			.replace(GIT_SUFFIX, "");
		if (!GITHUB_REPOSITORY.test(repo)) return null;
		const parts = skillPath.split("/");
		if (
			parts.some(
				(part) =>
					!part ||
					part === "." ||
					part === ".." ||
					part.includes("\\"),
			)
		)
			return null;
		return `${url.origin}${repo}/blob/${encodeURIComponent(gitRef || "HEAD")}/${parts.map(encodeURIComponent).join("/")}`;
	} catch {
		return null;
	}
}
