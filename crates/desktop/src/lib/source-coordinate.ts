/**
 * The ONE key a source is addressed by across the desktop: the lock's clone
 * URL when it recorded one, else the bare `source` id. The source diff, the
 * credential binding, apply-update batches, check-updates token forwarding and
 * the source deep link all key on it — a second spelling would forward a token
 * under a key the other paths never send.
 *
 * `||`, not `??`: a lock entry with no recorded URL carries `sourceUrl` as the
 * EMPTY STRING sentinel, not null — `??` would let that through as the key.
 */
export function sourceCoordinate(row: {
	source: string;
	sourceUrl?: string | null;
}): string {
	return row.sourceUrl || row.source;
}
