import {
	Avatar,
	Button,
	Card,
	Input,
	Switch,
	TextField,
	toast,
} from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getName, getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import {
	disable as disableAutostart,
	enable as enableAutostart,
	isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";
import { openUrl } from "@tauri-apps/plugin-opener";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useLastSkillCheck } from "../../hooks/use-last-skill-check";
import { dispatchOnboardingCommand } from "../../lib/onboarding";
import { getAghubCliPath, setAghubCliPath } from "../../lib/store";
import { isWindows } from "../../lib/platform";

export default function ApplicationPanel() {
	const { t } = useTranslation();
	const queryClient = useQueryClient();
	const isWindowsOS = isWindows();

	const { data: appInfo } = useQuery({
		queryKey: ["app-info"],
		queryFn: async () => {
			const name = await getName();
			const version = await getVersion();
			return { name, version };
		},
	});

	const { data: autostartEnabled = false, isPending: isAutostartLoading } =
		useQuery({
			queryKey: ["windows-autostart"],
			queryFn: isAutostartEnabled,
			enabled: isWindowsOS,
		});

	const autostartMutation = useMutation({
		mutationFn: async (enabled: boolean) => {
			if (enabled) {
				await enableAutostart();
			} else {
				await disableAutostart();
			}
			return enabled;
		},
		onSuccess: (enabled) => {
			queryClient.invalidateQueries({
				queryKey: ["windows-autostart"],
			});
			toast.success(
				enabled
					? t("settingsAutostartEnabled")
					: t("settingsAutostartDisabled"),
			);
		},
		onError: (error) => {
			toast.danger(
				error instanceof Error
					? error.message
					: t("settingsAutostartError"),
			);
		},
	});

	const checkMutation = useMutation({
		mutationFn: async () => {
			const update = await check();
			if (update) {
				return {
					available: true,
					version: update.version,
					currentVersion: update.currentVersion,
				};
			}
			return { available: false };
		},
	});

	const downloadMutation = useMutation({
		mutationFn: async () => {
			const update = await check();
			if (!update) throw new Error("No update available");

			await update.downloadAndInstall();
		},
		onSuccess: () => {
			toast.success(t("updateInstalledSuccess"), {
				timeout: 0,
				actionProps: {
					onPress: () => relaunch(),
					variant: "tertiary",
					children: t("restartNow"),
				},
				description: t("restartToUpdate"),
			});
		},
		onError: (error) => {
			toast.danger(`${t("updateError")}: ${error.message}`);
		},
	});

	const handleCheckUpdates = () => {
		checkMutation.mutate();
	};

	const handleDownloadAndInstall = () => {
		downloadMutation.mutate();
	};

	const updateCheckResult = checkMutation.data;
	const isChecking = checkMutation.isPending;
	const isDownloading = downloadMutation.isPending;
	const hasError = checkMutation.isError || downloadMutation.isError;
	const errorMessage =
		checkMutation.error?.message || downloadMutation.error?.message;

	const teamMembers = [
		{
			name: "AkaraChen",
			role: t("headDev"),
			avatar: "https://avatars.githubusercontent.com/u/85140972?v=4",
			githubUrl: "https://github.com/AkaraChen",
		},
		{
			name: "Flacier",
			role: t("developer"),
			avatar: "https://avatars.githubusercontent.com/u/48170241?v=4",
			githubUrl: "https://github.com/Fldicoahkiin",
		},
		{
			name: "danielchim",
			role: t("designer"),
			avatar: "https://avatars.githubusercontent.com/u/12156547?v=4",
			githubUrl: "https://github.com/danielchim",
		},
	];

	return (
		<div className="space-y-4">
			<Card className="p-0">
				<Card.Content className="space-y-4 p-4">
					<div className="flex items-center justify-between">
						<div className="space-y-0.5">
							<span className="text-sm font-medium text-(--foreground)">
								{t("appName")}
							</span>
							<span className="block text-xs text-muted">
								{appInfo?.name ?? "aghub"}
							</span>
						</div>
					</div>

					<div className="flex items-center justify-between">
						<div className="space-y-0.5">
							<span className="text-sm font-medium text-(--foreground)">
								{t("version")}
							</span>
							<span className="block text-xs text-muted">
								{appInfo?.version ?? "0.1.0"}
							</span>
						</div>
					</div>

					<div className="flex items-center justify-between">
						<div className="space-y-0.5">
							<span className="text-sm font-medium text-(--foreground)">
								{t("updates")}
							</span>
							<span className="block text-xs text-muted">
								{hasError &&
									`${t("updateError")}: ${errorMessage}`}
								{isChecking && t("checkingForUpdates")}
								{isDownloading && t("downloadingUpdate")}
								{!isChecking &&
									!isDownloading &&
									!hasError &&
									updateCheckResult?.available &&
									t("updateAvailable", {
										version: updateCheckResult.version,
									})}
								{!isChecking &&
									!isDownloading &&
									!hasError &&
									updateCheckResult &&
									!updateCheckResult.available &&
									t("noUpdatesAvailable")}
								{!isChecking &&
									!isDownloading &&
									!hasError &&
									!updateCheckResult &&
									t("clickToCheckUpdates")}
							</span>
						</div>
						<div className="flex gap-2">
							{!updateCheckResult && (
								<Button
									variant="secondary"
									size="sm"
									onPress={handleCheckUpdates}
									isDisabled={isChecking || isDownloading}
								>
									{t("checkForUpdates")}
								</Button>
							)}
							{updateCheckResult &&
								!updateCheckResult.available && (
									<Button
										variant="secondary"
										size="sm"
										onPress={handleCheckUpdates}
										isDisabled={isChecking || isDownloading}
									>
										{t("checkAgain")}
									</Button>
								)}
							{updateCheckResult?.available && (
								<Button
									variant="primary"
									size="sm"
									onPress={handleDownloadAndInstall}
									isDisabled={isDownloading}
								>
									{t("downloadAndInstall")}
								</Button>
							)}
						</div>
					</div>

					{isWindowsOS ? (
						<div className="flex items-center justify-between gap-4">
							<div className="space-y-0.5">
								<span className="text-sm font-medium text-(--foreground)">
									{t("settingsAutostartHeading")}
								</span>
								<span className="block text-xs text-muted">
									{t("settingsAutostartDescription")}
								</span>
							</div>
							<Switch
								isSelected={autostartEnabled}
								onChange={(checked) =>
									autostartMutation.mutate(checked)
								}
								isDisabled={
									isAutostartLoading ||
									autostartMutation.isPending
								}
								aria-label={t("settingsAutostartToggleLabel")}
							>
								<Switch.Control>
									<Switch.Thumb />
								</Switch.Control>
							</Switch>
						</div>
					) : null}

					<SkillCheckScheduleRow />

					<div className="flex items-center justify-between">
						<div className="space-y-0.5">
							<span className="text-sm font-medium text-(--foreground)">
								{t("onboarding")}
							</span>
							<span className="block text-xs text-muted">
								{t("onboardingDescription")}
							</span>
						</div>
						<div className="flex gap-2">
							<Button
								variant="secondary"
								size="sm"
								onPress={() =>
									dispatchOnboardingCommand({
										type: "show-welcome",
									})
								}
							>
								{t("showWelcome")}
							</Button>
							<Button
								variant="secondary"
								size="sm"
								onPress={() =>
									dispatchOnboardingCommand({
										type: "start-tour",
										tour: "product-map",
									})
								}
							>
								{t("replayAppTour")}
							</Button>
							<Button
								variant="secondary"
								size="sm"
								onPress={() =>
									dispatchOnboardingCommand({
										type: "start-tour",
										tour: "project-workflow",
									})
								}
							>
								{t("replayProjectTour")}
							</Button>
						</div>
					</div>
				</Card.Content>
			</Card>

			<Card className="p-0">
				<Card.Content className="p-4">
					<span className="text-sm font-medium text-(--foreground)">
						{t("team")}
					</span>
					<div className="mt-4 grid grid-cols-3 gap-4">
						{teamMembers.map((member) => (
							<button
								key={member.name}
								type="button"
								className="flex flex-col items-center text-center cursor-pointer"
								onClick={() => openUrl(member.githubUrl)}
							>
								<Avatar size="lg">
									<Avatar.Image
										src={member.avatar}
										alt={member.name}
									/>
								</Avatar>
								<span className="mt-2 text-sm font-medium">
									{member.name}
								</span>
								<span className="text-xs text-muted">
									{member.role}
								</span>
							</button>
						))}
					</div>
				</Card.Content>
			</Card>
		</div>
	);
}

interface SkillCheckScheduleStatus {
	supported: boolean;
	enabled: boolean;
	cliPath: string | null;
	sidecarPath: string;
}

function SkillCheckScheduleRow() {
	const { t } = useTranslation();
	const queryClient = useQueryClient();
	const [pathDraft, setPathDraft] = useState("");

	const { data: status, isPending } = useQuery({
		queryKey: ["skill-check-schedule"],
		queryFn: async () =>
			invoke<SkillCheckScheduleStatus>("get_skill_check_schedule"),
		retry: false,
	});

	const { data: last } = useLastSkillCheck();

	// An explicit path the user pointed at. Checked BEFORE PATH resolution, so
	// a GUI app whose PATH lacks the CLI is still usable without a relaunch.
	const { data: storedCliPath } = useQuery({
		queryKey: ["aghubCliPath"],
		queryFn: getAghubCliPath,
	});
	const effectiveCliPath = storedCliPath ?? status?.cliPath ?? null;

	const savePath = useMutation({
		mutationFn: async (path: string) => {
			// Validated by the backend (must be an existing file) before it is
			// stored, so a typo cannot silently disable the schedule later.
			const resolved = await invoke<string>("resolve_aghub_cli", {
				explicit: path,
			});
			await setAghubCliPath(resolved);
			return resolved;
		},
		onSuccess: (resolved) => {
			queryClient.setQueryData(["aghubCliPath"], resolved);
			setPathDraft("");
			toast.success(t("skillCheckScheduleCliPathSaved"));
		},
		onError: (error) => {
			toast.danger(
				error instanceof Error ? error.message : String(error),
			);
		},
	});

	const mutation = useMutation({
		mutationFn: async (enabled: boolean) =>
			invoke<SkillCheckScheduleStatus>("set_skill_check_schedule", {
				enabled,
				cliPath: effectiveCliPath,
			}),
		onSuccess: (next) => {
			queryClient.setQueryData(["skill-check-schedule"], next);
			toast.success(
				next.enabled
					? t("skillCheckScheduleEnabled")
					: t("skillCheckScheduleDisabled"),
			);
		},
		onError: (error) => {
			toast.danger(
				error instanceof Error
					? error.message
					: t("skillCheckScheduleError"),
			);
		},
	});

	// v1 registers a systemd --user timer; macOS/Windows have no backend yet, so
	// the row is hidden rather than offering a switch whose flip always errors.
	if (status != null && !status.supported) return null;

	const cliMissing = status != null && !effectiveCliPath;
	const summary = last
		? t("skillCheckScheduleLast", {
				available: last.updateAvailable ?? 0,
				failed: last.failed ?? 0,
				when: last.finishedAt ?? "",
			})
		: t("skillCheckScheduleNever");
	const needsAuth = last?.needsAuth ?? 0;

	return (
		<div className="flex items-start justify-between gap-4">
			<div className="space-y-0.5">
				<span className="text-sm font-medium text-(--foreground)">
					{t("skillCheckScheduleHeading")}
				</span>
				<span className="block text-xs text-muted">
					{cliMissing
						? t("skillCheckScheduleNeedCli")
						: t("skillCheckScheduleDescription")}
				</span>
				<span className="block text-xs text-muted">{summary}</span>
				{needsAuth > 0 ? (
					<span className="block text-xs text-muted">
						{t("skillCheckScheduleNeedsAuth", { count: needsAuth })}
					</span>
				) : null}
				{cliMissing ? (
					<div className="flex items-center gap-2 pt-1">
						<TextField
							className="w-72"
							aria-label={t("skillCheckScheduleCliPathLabel")}
						>
							<Input
								value={pathDraft}
								variant="secondary"
								placeholder={t(
									"skillCheckScheduleCliPathPlaceholder",
								)}
								onChange={(e) => setPathDraft(e.target.value)}
							/>
						</TextField>
						<Button
							variant="secondary"
							size="sm"
							isDisabled={
								pathDraft.trim() === "" || savePath.isPending
							}
							onPress={() => savePath.mutate(pathDraft.trim())}
						>
							{t("skillCheckScheduleCliPathSave")}
						</Button>
					</div>
				) : null}
			</div>
			<Switch
				isSelected={status?.enabled ?? false}
				onChange={(checked) => mutation.mutate(checked)}
				isDisabled={isPending || mutation.isPending || cliMissing}
				aria-label={t("skillCheckScheduleHeading")}
				// The compound root reserves a label slot and is wider than the
				// visible control, which throws off this row's right edge.
				className="w-10 shrink-0"
			>
				<Switch.Control>
					<Switch.Thumb />
				</Switch.Control>
			</Switch>
		</div>
	);
}
