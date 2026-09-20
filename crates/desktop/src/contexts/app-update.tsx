import type { ReactNode } from "react";
import { createContext } from "react";
import type { AppUpdatePhase } from "../lib/app-update";

export interface AppUpdateState {
	phase: AppUpdatePhase;
	/** Set once a check has answered; `null` before the first check. */
	available: { version: string; currentVersion: string } | null;
	/** `null` while `contentLength` is unknown — render indeterminate. */
	percent: number | null;
	error: string | null;
}

export interface AppUpdateContextValue extends AppUpdateState {
	checkForUpdate: () => void;
	downloadAndInstall: () => void;
	restart: () => void;
}

/**
 * The app-update flow, hoisted out of the About panel.
 *
 * It lives in a provider mounted for the app's whole life because the STATE
 * has to outlive the panel: the download keeps running when the user navigates
 * away, and a `useMutation` owned by the panel is destroyed with it. Coming
 * back showed "Check for updates" over a download in flight, and the success
 * toast never fired at all if the user had left the page.
 */
export const AppUpdateContext = createContext<AppUpdateContextValue | null>(
	null,
);

export interface AppUpdateProviderProps {
	children: ReactNode;
}
