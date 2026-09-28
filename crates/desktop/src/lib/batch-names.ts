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

/**
 * Send `names` as request-sized batches, in order, and flatten the per-name
 * results.
 *
 * The chunking lives HERE, not inline at the call site, because this app has
 * no component-level test runner: an inline loop could be removed with every
 * test still green. Sequential on purpose — the server serializes them anyway.
 *
 * `onChunk` receives each chunk's rows AS THEY ARRIVE, so a throw on a later
 * chunk does not report names the server already wrote as failed.
 */
export async function sendInBatches<T>(
	names: string[],
	send: (chunk: string[]) => Promise<T[]>,
	onChunk?: (rows: T[]) => void,
): Promise<T[]> {
	const results: T[] = [];
	for (const chunk of chunkNames(names)) {
		const rows = await send(chunk);
		onChunk?.(rows);
		results.push(...rows);
	}
	return results;
}
