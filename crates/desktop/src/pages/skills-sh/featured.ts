import type { MarketSkill } from "../../generated/dto";

/** Install API source shape: `github/owner/repo`. Tree URLs are rejected. */
export const INSTALL_SOURCE_RE = /^github\/[^/]+\/[^/]+$/;

export interface FeaturedSkill {
	name: string;
	slug: string;
	summary: string;
	source: string;
	author?: string;
	installs?: number;
}

interface FeaturedCatalogFile {
	skills: FeaturedSkill[];
}

export function isInstallShapedSource(source: string): boolean {
	return INSTALL_SOURCE_RE.test(source) && !source.includes("/tree/");
}

export function parseFeaturedCatalog(raw: unknown): FeaturedSkill[] {
	if (raw === null || typeof raw !== "object") {
		throw new TypeError("featured catalog must be an object");
	}
	const skills = (raw as FeaturedCatalogFile).skills;
	if (!Array.isArray(skills)) {
		throw new TypeError("featured catalog must have a skills array");
	}
	const parsed: FeaturedSkill[] = [];
	for (const entry of skills) {
		if (entry === null || typeof entry !== "object") {
			throw new TypeError("featured catalog entry must be an object");
		}
		const name = requiredString(entry, "name");
		const slug = requiredString(entry, "slug");
		const summary = requiredString(entry, "summary");
		const source = requiredString(entry, "source");
		if (!isInstallShapedSource(source)) {
			throw new Error(
				`featured catalog entry ${name} has non-install source: ${source}`,
			);
		}
		const author =
			"author" in entry && typeof entry.author === "string"
				? entry.author
				: undefined;
		const installs =
			"installs" in entry && typeof entry.installs === "number"
				? entry.installs
				: undefined;
		parsed.push({ name, slug, summary, source, author, installs });
	}
	return parsed;
}

export function asMarketSkill(entry: FeaturedSkill): MarketSkill {
	return {
		name: entry.name,
		slug: entry.slug,
		source: entry.source,
		installs: entry.installs ?? 0,
		author: entry.author ?? null,
	};
}

function requiredString(
	entry: object,
	key: "name" | "slug" | "summary" | "source",
): string {
	const value = (entry as Record<string, unknown>)[key];
	if (typeof value !== "string" || value.trim() === "") {
		throw new Error(`featured catalog entry missing ${key}`);
	}
	return value;
}
