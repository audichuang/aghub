import { toast } from "@heroui/react";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type {
	AppUpdateProviderProps,
	AppUpdateState,
} from "../contexts/app-update";
import { AppUpdateContext } from "../contexts/app-update";
import { canStartUpdateWork, downloadPercent } from "../lib/app-update";

const IDLE: AppUpdateState = {
	phase: "idle",
	available: null,
	percent: null,
	error: null,
};

export function AppUpdateProvider({ children }: AppUpdateProviderProps) {
	const { t } = useTranslation();
	const [state, setState] = useState<AppUpdateState>(IDLE);
	// The phase is read inside async callbacks to decide whether a second press
	// may start work. Reading it from `state` would capture the value from the
	// render that created the callback, which is exactly the stale read that
	// lets two downloads start.
	const phaseRef = useRef(state.phase);
	const apply = useCallback((next: AppUpdateState) => {
		phaseRef.current = next.phase;
		setState(next);
	}, []);

	const checkForUpdate = useCallback(async () => {
		if (!canStartUpdateWork(phaseRef.current)) return;
		apply({ ...IDLE, phase: "checking" });
		try {
			const update = await check();
			if (!update) {
				apply({ ...IDLE, phase: "up-to-date" });
				return;
			}
			apply({
				...IDLE,
				phase: "available",
				available: {
					version: update.version,
					currentVersion: update.currentVersion,
				},
			});
		} catch (error) {
			apply({
				...IDLE,
				phase: "error",
				error: error instanceof Error ? error.message : String(error),
			});
		}
	}, [apply]);

	const downloadAndInstall = useCallback(async () => {
		if (!canStartUpdateWork(phaseRef.current)) return;
		const available = state.available;
		apply({ ...IDLE, phase: "downloading", available });
		try {
			const update = await check();
			if (!update) throw new Error(t("noUpdatesAvailable"));

			let contentLength: number | undefined;
			let downloaded = 0;
			await update.downloadAndInstall((event) => {
				if (event.event === "Started") {
					contentLength = event.data.contentLength;
				} else if (event.event === "Progress") {
					downloaded += event.data.chunkLength;
				}
				phaseRef.current = "downloading";
				setState({
					phase: "downloading",
					available,
					percent: downloadPercent(downloaded, contentLength),
					error: null,
				});
			});

			apply({ ...IDLE, phase: "installed", available });
			// Fired from the provider, not the panel: the user is usually on
			// another page by the time a slow download lands, and a toast owned
			// by an unmounted panel never appeared at all.
			toast.success(t("updateInstalledSuccess"), {
				timeout: 0,
				actionProps: {
					onPress: () => relaunch(),
					variant: "tertiary",
					children: t("restartNow"),
				},
				description: t("restartToUpdate"),
			});
		} catch (error) {
			const message =
				error instanceof Error ? error.message : String(error);
			apply({ ...IDLE, phase: "error", available, error: message });
			toast.danger(`${t("updateError")}: ${message}`);
		}
	}, [apply, state.available, t]);

	return (
		<AppUpdateContext
			value={{
				...state,
				checkForUpdate: () => void checkForUpdate(),
				downloadAndInstall: () => void downloadAndInstall(),
				restart: () => void relaunch(),
			}}
		>
			{children}
		</AppUpdateContext>
	);
}
