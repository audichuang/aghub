import type { ConnectionContextValue } from "../contexts/connection";
import { useConnectionContext } from "../contexts/connection";

export function useConnection(): ConnectionContextValue {
	return useConnectionContext();
}
