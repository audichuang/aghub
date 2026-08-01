import { MAX_BATCH_NAMES } from "../generated/dto/limits.ts";

/**
 * Split `names` into request-sized chunks, preserving order.
 *
 * The skills API caps one `apply-updates` body's `names` and REFUSES an
 * oversized request rather than truncating it, so a client that sends every
 * outdated skill of a large Source in one body gets nothing updated at all.
 * The cap itself is generated from the server (`generated/dto/limits.ts`) —
 * never re-declare it here.
 */
export function chunkNames(names: string[]): string[][] {
	const chunks: string[][] = [];
	for (let index = 0; index < names.length; index += MAX_BATCH_NAMES) {
		chunks.push(names.slice(index, index + MAX_BATCH_NAMES));
	}
	return chunks;
}
