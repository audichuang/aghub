/**
 * Hard cap the skills API puts on one `apply-updates` body's `names`
 * (`MAX_BATCH_NAMES` in `crates/api/src/routes/skills_update.rs`). An oversized
 * request is REFUSED, not truncated, so a client that sends every outdated
 * skill of a large Source in one body gets nothing updated at all.
 */
export const MAX_BATCH_NAMES = 256;

/** Split `names` into request-sized chunks, preserving order. */
export function chunkNames(
	names: string[],
	size: number = MAX_BATCH_NAMES,
): string[][] {
	if (names.length === 0) return [];
	const chunks: string[][] = [];
	for (let index = 0; index < names.length; index += size) {
		chunks.push(names.slice(index, index + size));
	}
	return chunks;
}
