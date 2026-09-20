import { use } from "react";
import type { AppUpdateContextValue } from "../contexts/app-update";
import { AppUpdateContext } from "../contexts/app-update";

export function useAppUpdate(): AppUpdateContextValue {
	const value = use(AppUpdateContext);
	if (value === null) {
		throw new Error("useAppUpdate must be used inside <AppUpdateProvider>");
	}
	return value;
}
