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
 * The chunking lives HERE rather than inline at the call site so a regression
 * to one oversized request is observable: a component-level test runner does
 * not exist in this app, so an inline loop could be removed with every test
 * still green.
 *
 * Sequential on purpose — each batch occupies a mutation worker for its whole
 * span, and the server serializes them anyway.
 */
export async function sendInBatches<T>(
	names: string[],
	send: (chunk: string[]) => Promise<T[]>,
): Promise<T[]> {
	const results: T[] = [];
	for (const chunk of chunkNames(names)) {
		results.push(...(await send(chunk)));
	}
	return results;
}
