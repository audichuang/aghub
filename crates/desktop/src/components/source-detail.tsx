import {
	ArrowDownTrayIcon,
	ArrowPathIcon,
	CheckCircleIcon,
	ChevronDownIcon,
	ChevronRightIcon,
	ClipboardDocumentIcon,
	ExclamationTriangleIcon,
	FolderIcon,
	GlobeAltIcon,
	LockClosedIcon,
	TrashIcon,
} from "@heroicons/react/24/solid";
import { Alert, Button, Chip, Spinner, toast } from "@heroui/react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { SourceCredentialBindingDialog } from "./source-credential-binding-dialog";
import { SourceSkillRow } from "./source-skill-row";
import type { SourceSkillDiff } from "../generated/dto";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { useApi } from "../hooks/use-api";
import { useApplyAllSkillUpdates } from "../hooks/use-apply-all-skill-updates";
import { useGitForwarding } from "../hooks/use-git-forwarding";
import {
	groupAgentsBySlot,
	supportsSkillMutation,
} from "../lib/agent-capabilities";
import {
	allSkillPaths,
	selectedSkills,
	toggleSkillPath,
} from "../lib/source-skill-selection";
import { cn } from "../lib/utils";
import { useSkillCoverage } from "../requests/agents";
import { queryKeys } from "../requests/keys";
import { applySkillUpdateMutationOptions } from "../requests/skills";
import { sourceDiffQueryOptions } from "../requests/sources";

const SKILL_FILE_SUFFIX_RE = /\/SKILL\.md$/;
const EMPTY_DIFFS: SourceSkillDiff[] = [];

// ponytail: elapsed-seconds note so a long fetch reads as "working", not "hung".
// Render with key={source} so switching source remounts it and the counter
// resets. setState only fires in the interval callback (not the effect body),
// and there is no Date.now() in render — keeps it lint-pure.
function CheckingElapsed() {
	const [seconds, setSeconds] = useState(0);
	useEffect(() => {
		const id = setInterval(() => setSeconds((s) => s + 1), 1000);
		return () => clearInterval(id);
	}, []);
	return seconds >= 2 ? <span>{` ${seconds}s`}</span> : null;
}

export interface SourceRow {
	source: string;
	sourceUrl: string;
	sourceType: string;
	isPrivate?: boolean;
	credentialStatus?: string;
	skillCount: number;
	rowScope: "global" | "project";
	projectRoot?: string;
	projectName?: string;
}

interface SourceDetailProps {
	row: SourceRow;
	onImport: () => void;
}

// ─── Summary bar counts ──────────────────────────────────────────────────────

interface SummaryBarProps {
	notInstalledCount: number;
	outdatedCount: number;
	renamedCount: number;
	uncheckableCount: number;
	currentCount: number;
	isLoading: boolean;
}

function SummaryBar({
	notInstalledCount,
	outdatedCount,
	renamedCount,
	uncheckableCount,
	currentCount,
	isLoading,
}: SummaryBarProps) {
	const { t } = useTranslation();
	if (isLoading) return null;

	return (
		<div className="flex flex-wrap gap-2 rounded-lg bg-surface-secondary px-4 py-2.5">
			{outdatedCount > 0 && (
				<Chip size="sm" variant="secondary">
					{t("summaryUpdatable", { count: outdatedCount })}
				</Chip>
			)}
			{notInstalledCount > 0 && (
				<Chip size="sm" variant="secondary">
					{t("summaryInstallable", { count: notInstalledCount })}
				</Chip>
			)}
			{renamedCount > 0 && (
				<Chip size="sm" variant="secondary">
					{t("summaryRenamed", { count: renamedCount })}
				</Chip>
			)}
			{uncheckableCount > 0 && (
				<Chip size="sm" variant="secondary">
					{t("summaryUnchecked", { count: uncheckableCount })}
				</Chip>
			)}
			{currentCount > 0 && (
				<Chip size="sm" variant="secondary">
					{t("summaryLatest", { count: currentCount })}
				</Chip>
			)}
		</div>
	);
}

// ─── SkillSection (local — collapsible section with skill rows) ──────────────

interface SkillSectionProps {
	title: string;
	icon: React.ReactNode;
	skills: SourceSkillDiff[];
	expandedSkillPath: string | null;
	onToggleSkill: (skillPath: string | null) => void;
	muted?: boolean;
	defaultCollapsed?: boolean;
}

function SkillSection({
	title,
	icon,
	skills,
	expandedSkillPath,
	onToggleSkill,
	muted = false,
	defaultCollapsed = false,
}: SkillSectionProps) {
	const [collapsed, setCollapsed] = useState(defaultCollapsed);

	if (skills.length === 0) return null;

	return (
		<section>
			<div className="mb-2 flex items-center justify-between gap-3">
				<button
					type="button"
					className="flex min-w-0 items-center gap-2"
					onClick={() => setCollapsed((c) => !c)}
					aria-expanded={!collapsed}
				>
					{collapsed ? (
						<ChevronRightIcon className="size-4 shrink-0 text-muted" />
					) : (
						<ChevronDownIcon className="size-4 shrink-0 text-muted" />
					)}
					{icon}
					<h2
						className={cn(
							"truncate text-sm font-semibold",
							muted ? "text-muted" : "text-foreground",
						)}
					>
						{title}
					</h2>
					<span className="shrink-0 text-xs text-muted">
						{skills.length}
					</span>
				</button>
			</div>
			{!collapsed && (
				<ul className="overflow-hidden rounded-lg border border-border">
					{skills.map((skill) => (
						<SourceSkillRow
							key={skill.skillPath}
							skill={skill}
							isExpanded={expandedSkillPath === skill.skillPath}
							onToggle={() =>
								onToggleSkill(
									expandedSkillPath === skill.skillPath
										? null
										: skill.skillPath,
								)
							}
							muted={muted}
						/>
					))}
				</ul>
			)}
		</section>
	);
}

// ─── SourceOrphanLockAlert / SourceEmptyState ─────────────────────────────────

interface SourceOrphanLockAlertProps {
	prunedCount: number;
	isChecking: boolean;
	isCleaning: boolean;
	onClean: () => void;
}

function SourceOrphanLockAlert({
	prunedCount,
	isChecking,
	isCleaning,
	onClean,
}: SourceOrphanLockAlertProps) {
	const { t } = useTranslation();
	const orphanHint =
		prunedCount === 1
			? t("sourceOrphanHintOne", { count: prunedCount })
			: t("sourceOrphanHintMany", { count: prunedCount });

	return (
		<Alert status="warning">
			<Alert.Indicator />
			<Alert.Content>
				<Alert.Title>{t("sourceOrphanTitle")}</Alert.Title>
				<Alert.Description>{orphanHint}</Alert.Description>
				<div className="mt-3">
					<Button
						size="sm"
						variant="secondary"
						isDisabled={isChecking || isCleaning}
						onPress={onClean}
					>
						<TrashIcon className="size-3.5" />
						{isCleaning
							? t("sourceCleaningOrphans")
							: t("sourceCleanOrphans")}
					</Button>
				</div>
			</Alert.Content>
		</Alert>
	);
}

interface SourceEmptyStateProps {
	prunedCount: number;
	isChecking: boolean;
	isCleaning: boolean;
	hasError: boolean;
	onClean: () => void;
	onRetry: () => void;
}

function SourceEmptyState({
	prunedCount,
	isChecking,
	isCleaning,
	hasError,
	onClean,
	onRetry,
}: SourceEmptyStateProps) {
	const { t } = useTranslation();
	const hasOrphans = prunedCount > 0;

	if (hasError) {
		return (
			<Alert status="danger">
				<Alert.Indicator />
				<Alert.Content>
					<Alert.Title>
						{t("sourcePrunePreviewErrorTitle")}
					</Alert.Title>
					<Alert.Description>
						{t("sourcePrunePreviewErrorHint")}
					</Alert.Description>
					<div className="mt-3">
						<Button size="sm" variant="secondary" onPress={onRetry}>
							{t("retry")}
						</Button>
					</div>
				</Alert.Content>
			</Alert>
		);
	}

	if (hasOrphans) {
		return (
			<SourceOrphanLockAlert
				prunedCount={prunedCount}
				isChecking={isChecking}
				isCleaning={isCleaning}
				onClean={onClean}
			/>
		);
	}

	return (
		<Alert status="warning">
			<Alert.Indicator />
			<Alert.Content>
				<Alert.Title>{t("sourceEmptyDiffTitle")}</Alert.Title>
				<Alert.Description>
					{isChecking
						? t("sourceCheckingOrphans")
						: t("sourceEmptyDiffHint")}
				</Alert.Description>
			</Alert.Content>
		</Alert>
	);
}

// ─── SourceDetail (main export) ──────────────────────────────────────────────

export function SourceDetail({ row, onImport }: SourceDetailProps) {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const { forSource: forwardForSource } = useGitForwarding();
	const { availableAgents } = useAgentAvailability();
	const [expandedSkillPath, setExpandedSkillPath] = useState<string | null>(
		null,
	);
	const [isInstallingAll, setIsInstallingAll] = useState(false);
	const [isDeletingAllRemoved, setIsDeletingAllRemoved] = useState(false);
	// Count of skills processed so far by the active batch (update / clean-up)
	// loop, so the button can show X/Y instead of a static "…". One counter is
	// enough: only one batch runs at a time.
	const [batchDone, setBatchDone] = useState(0);
	const [installingSkillPath, setInstallingSkillPath] = useState<
		string | null
	>(null);
	const [selectedInstallSkillPaths, setSelectedInstallSkillPaths] = useState<
		Set<string>
	>(() => new Set());
	const [isCredentialDialogOpen, setIsCredentialDialogOpen] = useState(false);

	// P1-c: use the recorded clone URL as the network/credential coordinate.
	// `row.source` is the row's ORIGIN (`host[:port]/path`) — a unique identity
	// this list is keyed on, and resolvable back to a clone URL, but not itself a
	// fetch coordinate. Prefer the recorded URL; fall back for entries that have
	// none (e.g. local sources).
	const diffSource = row.sourceUrl || row.source;

	const { data, isLoading, isFetching } = useQuery(
		sourceDiffQueryOptions({
			api,
			source: diffSource,
			scope: row.rowScope,
			projectRoot:
				row.rowScope === "project" ? row.projectRoot : undefined,
			enabled: true,
			forwardForSource,
		}),
	);

	const grouped = useMemo(() => {
		const byState = new Map<string, SourceSkillDiff[]>();
		for (const skill of data?.skills ?? []) {
			const existing = byState.get(skill.state) ?? [];
			byState.set(skill.state, [...existing, skill]);
		}
		return byState;
	}, [data]);

	const notInstalled = grouped.get("notInstalled") ?? EMPTY_DIFFS;
	const outdated = grouped.get("installedOutdated") ?? EMPTY_DIFFS;
	const renamed = grouped.get("renamed") ?? EMPTY_DIFFS;
	const removed = grouped.get("removed") ?? EMPTY_DIFFS;
	const deprecated = grouped.get("deprecated") ?? EMPTY_DIFFS;
	const installedDeprecated = useMemo(
		() => deprecated.filter((skill) => skill.installedPaths.length > 0),
		[deprecated],
	);
	const current = grouped.get("installedCurrent") ?? EMPTY_DIFFS;
	const uncheckable = grouped.get("uncheckable") ?? EMPTY_DIFFS;

	const selectedInstallSkills = useMemo(
		() => selectedSkills(notInstalled, selectedInstallSkillPaths),
		[notInstalled, selectedInstallSkillPaths],
	);
	const allInstallSkillPaths = useMemo(
		() => allSkillPaths(notInstalled),
		[notInstalled],
	);
	const selectedInstallCount = selectedInstallSkills.length;
	const hasSelectedInstallSkills = selectedInstallCount > 0;
	const allInstallSkillsSelected =
		notInstalled.length > 0 && selectedInstallCount === notInstalled.length;
	const isChecking = isLoading || isFetching;
	const hasVisibleSkills = (data?.skills.length ?? 0) > 0;
	const updateScope = row.rowScope;
	const updateProjectRoot =
		row.rowScope === "project" ? (row.projectRoot ?? null) : null;
	const shouldCheckOrphans =
		!isLoading && !isFetching && Boolean(data) && !data?.needsCredential;

	const installableAgents = useMemo(
		() =>
			availableAgents.filter(
				(agent) =>
					agent.isUsable && supportsSkillMutation(agent, updateScope),
			),
		[availableAgents, updateScope],
	);
	const installableAgentIds = useMemo(
		() => installableAgents.map((agent) => agent.id),
		[installableAgents],
	);

	const { coverage, isLoading: isCoverageLoading } = useSkillCoverage(
		updateScope,
		updateProjectRoot,
	);
	const slotGroups = useMemo(
		() => groupAgentsBySlot(installableAgents, coverage),
		[installableAgents, coverage],
	);
	const linkTargets = useMemo(
		() => slotGroups.flatMap((g) => g.members),
		[slotGroups],
	);
	const sharedGroups = useMemo(
		() => slotGroups.filter((g) => g.shared),
		[slotGroups],
	);
	const linkTargetAgentIds = useMemo(
		() => linkTargets.map((a) => a.id),
		[linkTargets],
	);

	const applyUpdateMutation = useMutation(
		applySkillUpdateMutationOptions({
			api,
			queryClient,
			forwardForSource,
			onSuccess: async (result) => {
				if (!result.success) {
					toast.danger(result.error ?? t("skillUpdateApplyError"));
					return;
				}
				toast.success(t("skillSyncedSuccessfully"));
				await queryClient.invalidateQueries({
					queryKey: queryKeys.skills.sources.all(),
				});
			},
			onError: () => toast.danger(t("skillUpdateApplyError")),
		}),
	);

	const { applyAll, isApplying: isApplyingAll } = useApplyAllSkillUpdates();

	const prunePreviewQuery = useQuery({
		queryKey: queryKeys.skills.pruneLock(updateScope, updateProjectRoot),
		queryFn: () =>
			api.skills.pruneLock({
				scope: updateScope,
				projectRoot: updateProjectRoot,
				confirm: false,
			}),
		enabled: shouldCheckOrphans,
	});
	const orphanLockCount = prunePreviewQuery.data?.pruned.length ?? 0;

	const pruneLockMutation = useMutation({
		mutationFn: () =>
			api.skills.pruneLock({
				scope: updateScope,
				projectRoot: updateProjectRoot,
				confirm: true,
			}),
		onSuccess: async (result) => {
			if (result.error) {
				toast.danger(result.error);
				return;
			}
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.all(),
			});
			if (result.pruned.length === 0) {
				toast.success(t("sourceOrphansCleanedZero"));
			} else {
				toast.success(
					t("sourceOrphansCleanedMany", {
						count: result.pruned.length,
					}),
				);
			}
		},
		onError: () => toast.danger(t("sourcePruneFailed")),
	});

	const deleteInstalledSkillByName = async (name: string) => {
		if (installableAgentIds.length === 0) {
			throw new Error(t("sourceRemoveNoAgents"));
		}
		const result = await api.skills.delete(
			installableAgentIds[0],
			name,
			updateScope,
			updateProjectRoot ?? undefined,
			true,
		);
		if (result.error) {
			throw new Error(result.error);
		}
		// This helper's post-condition is "the skill is gone", so `absent` is a
		// success. `!executed` could not express that — nor tell it apart from
		// `kept`, where the shared master is still there and the skill is still
		// installed.
		if (result.outcome !== "removed" && result.outcome !== "absent") {
			throw new Error(t("sourceRemovedCleanFailed", { name }));
		}
	};

	const deleteRenamedSkillMutation = useMutation({
		mutationFn: async (skill: SourceSkillDiff) => {
			const oldName = skill.previousName;
			if (!oldName) {
				throw new Error("Missing previous name for renamed skill.");
			}
			await deleteInstalledSkillByName(oldName);
		},
		onSuccess: async (_data, skill) => {
			if (!skill.previousName) return;
			toast.success(
				t("sourceRenamedDeleted", { oldName: skill.previousName }),
			);
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.all(),
			});
		},
		onError: (error, skill) => {
			if (skill.previousName) {
				toast.danger(
					error instanceof Error
						? error.message
						: t("sourceRenamedDeleteFailed", {
								oldName: skill.previousName,
							}),
				);
				return;
			}
			toast.danger(t("sourcePruneFailed"));
		},
	});

	const deleteRemovedSkillMutation = useMutation({
		mutationFn: async (skill: SourceSkillDiff) => {
			await deleteInstalledSkillByName(skill.name);
		},
		onSuccess: async (_data, skill) => {
			toast.success(t("sourceRemovedCleaned", { name: skill.name }));
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.all(),
			});
		},
		onError: (error, skill) => {
			toast.danger(
				error instanceof Error
					? error.message
					: t("sourceRemovedCleanFailed", { name: skill.name }),
			);
		},
	});

	const copyRenamedInstallMutation = useMutation({
		mutationFn: async (skill: SourceSkillDiff) => {
			await writeText(`aghub-cli install ${skill.name}`);
		},
		onSuccess: (_data, skill) => {
			toast.success(
				t("sourceRenamedInstallCommandCopied", {
					newName: skill.name,
				}),
			);
		},
		onError: () => toast.danger(t("sourceCopyCommandFailed")),
	});

	const updateRequestFor = (skill: SourceSkillDiff) => ({
		name: skill.name,
		scope: updateScope,
		projectRoot: updateProjectRoot,
		confirm: true,
	});

	const applyOneUpdate = (skill: SourceSkillDiff) => {
		// P1-b/P1-c: forward the controller token, keyed by the clone URL.
		applyUpdateMutation.mutate({
			body: updateRequestFor(skill),
			sourceUrl: diffSource,
		});
	};

	const applyAllUpdates = async (skills: SourceSkillDiff[]) => {
		if (isDeletingAllRemoved) return;
		// Batching, ordering and the "a transport error proves nothing" rule
		// live in the shared hook — the agent view runs the same flow across
		// several sources and the two must not drift apart.
		const outcome = await applyAll([
			{
				source: diffSource,
				names: skills.map((skill) => skill.name),
				scope: updateScope,
				projectRoot: updateProjectRoot,
			},
		]);
		if (!outcome) return;
		if (outcome.unconfirmed) {
			// Chunks the server already answered are confirmed; only what came
			// after the failure is unknown.
			toast.danger(
				outcome.updated > 0
					? t("sourceUpdatePartialUnconfirmed", {
							count: outcome.updated,
						})
					: t("sourceUpdateUnconfirmed"),
				{ description: outcome.failureDescription },
			);
			return;
		}
		const failureCount =
			outcome.failures.length + outcome.definiteFailureCount;
		if (failureCount > 0) {
			// Per-row reasons are the only actionable part — a repointed
			// source or a skill missing upstream needs a different response
			// from the user than a network failure. Same as the per-row
			// button, which already surfaces `result.error`.
			toast.danger(
				failureCount === 1
					? t("sourceUpdateSomeFailedOne", { count: 1 })
					: t("sourceUpdateSomeFailedMany", { count: failureCount }),
				{
					description:
						outcome.failures[0]?.error ??
						outcome.failureDescription ??
						undefined,
				},
			);
			return;
		}
		toast.success(t("sourceUpdatesApplied", { count: outcome.updated }));
	};

	const deleteAllRemovedSkills = async (skills: SourceSkillDiff[]) => {
		if (
			skills.length === 0 ||
			isApplyingAll ||
			isDeletingAllRemoved ||
			deleteRemovedSkillMutation.isPending
		) {
			return;
		}
		if (installableAgentIds.length === 0) {
			toast.danger(t("sourceRemoveNoAgents"));
			return;
		}
		setIsDeletingAllRemoved(true);
		setBatchDone(0);
		let cleaned = 0;
		let failed = 0;
		try {
			for (const skill of skills) {
				try {
					await deleteInstalledSkillByName(skill.name);
					cleaned += 1;
				} catch {
					failed += 1;
				}
				setBatchDone(cleaned + failed);
			}
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.all(),
			});
			if (failed > 0) {
				toast.danger(
					failed === 1
						? t("sourceRemovedCleanSomeFailedOne", {
								count: failed,
							})
						: t("sourceRemovedCleanSomeFailedMany", {
								count: failed,
							}),
				);
			} else {
				toast.success(
					t("sourceRemovedCleanedMany", { count: cleaned }),
				);
			}
		} finally {
			setIsDeletingAllRemoved(false);
		}
	};

	const toggleInstallSkillSelection = (skill: SourceSkillDiff) => {
		setSelectedInstallSkillPaths((previous) =>
			toggleSkillPath(previous, skill.skillPath),
		);
	};

	const installPathFor = (skill: SourceSkillDiff) =>
		skill.skillPath === "SKILL.md"
			? "."
			: skill.skillPath.replace(SKILL_FILE_SUFFIX_RE, "");

	const installFromSource = async (skills: SourceSkillDiff[]) => {
		if (
			skills.length === 0 ||
			isInstallingAll ||
			installingSkillPath !== null
		) {
			return;
		}
		const installAll = skills.length > 1;
		if (installAll) {
			setIsInstallingAll(true);
		} else {
			setInstallingSkillPath(skills[0]?.skillPath ?? null);
		}

		try {
			// Resolve the forward header transiently (remote mode only); pinned
			// to the source's clone URL. Discarded after the request.
			const forwardHeaders = await forwardForSource(row.sourceUrl);
			const scan = await api.skills.gitScan(
				{
					url: row.sourceUrl,
					credential_id: null,
					branch: data?.gitRef ?? null,
					session_id: null,
				},
				forwardHeaders,
			);
			const wantedPaths = new Set(skills.map(installPathFor));
			const scanPaths = new Set(scan.skills.map((skill) => skill.path));
			const skillPaths = Array.from(wantedPaths).filter((path) =>
				scanPaths.has(path),
			);

			if (skillPaths.length !== wantedPaths.size) {
				throw new Error(t("sourceInstallFailed"));
			}

			// P3: install reuses the scan session's server-side cached token, so
			// no forward header is sent here (only the scan above carries it).
			const result = await api.skills.gitInstall({
				session_id: scan.session_id,
				skill_paths: skillPaths,
				agents: linkTargetAgentIds,
				scope: updateScope,
				project_root: updateProjectRoot,
			});
			const failed = result.results.filter((entry) => !entry.success);

			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.all(),
			});
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.sources.all(),
			});

			if (failed.length > 0) {
				toast.danger(
					failed.length === 1
						? t("sourceInstallSomeFailedOne", {
								count: failed.length,
							})
						: t("sourceInstallSomeFailedMany", {
								count: failed.length,
							}),
				);
			} else {
				toast.success(
					t("sourceInstalled", { count: skillPaths.length }),
				);
			}
		} catch (error) {
			toast.danger(
				error instanceof Error
					? error.message
					: t("sourceInstallFailed"),
			);
		} finally {
			setIsInstallingAll(false);
			setInstallingSkillPath(null);
		}
	};

	// Split uncheckable into auth-blocked (actionable) vs other (info-only)
	const uncheckableAuth = useMemo(
		() => uncheckable.filter((s) => s.reason === "auth"),
		[uncheckable],
	);
	const uncheckableNonAuth = useMemo(
		() => uncheckable.filter((s) => s.reason !== "auth"),
		[uncheckable],
	);

	// "Needs action" bucket: only states where user can take an action.
	// Non-auth uncheckable rows are NOT actionable — excluded from this list.
	const needsActionSkills = useMemo(
		() => [
			...outdated,
			...notInstalled,
			...renamed,
			...removed,
			...installedDeprecated,
			...uncheckableAuth,
		],
		[
			outdated,
			notInstalled,
			renamed,
			removed,
			installedDeprecated,
			uncheckableAuth,
		],
	);

	const hasNeedsAction = needsActionSkills.length > 0;

	const SourceIcon = row.sourceType === "local" ? FolderIcon : GlobeAltIcon;

	// P2 correction: use sourceUrl for dialog so host resolution works
	const credentialBindingSource = row.sourceUrl || row.source;

	return (
		<div className="flex h-full flex-col overflow-hidden">
			{/* Header */}
			<div className="relative z-10 flex items-start justify-between gap-3 border-b border-border p-4 [transform:translateZ(0)]">
				<div className="min-w-0">
					<div className="flex items-center gap-2">
						<SourceIcon className="size-5 shrink-0 text-muted" />
						<h1 className="truncate text-lg font-semibold text-foreground">
							{row.source}
						</h1>
						{row.isPrivate && (
							<LockClosedIcon
								className="size-3.5 shrink-0 text-muted"
								aria-label={t("privateRepo")}
							/>
						)}
						<Chip size="sm" variant="secondary">
							{row.rowScope === "global"
								? t("scopeGlobal")
								: `${t("scopeProject")} · ${row.projectName ?? ""}`}
						</Chip>
					</div>
					<p className="mt-1 truncate font-mono text-xs text-muted">
						{row.sourceUrl}
					</p>
				</div>
				<Button className="shrink-0" onPress={onImport}>
					<ArrowDownTrayIcon className="size-4" />
					{t("importFromThisSource")}
				</Button>
			</div>

			{/* Body */}
			<div className="min-h-0 flex-1 overflow-y-auto p-4 [transform:translateZ(0)]">
				{isChecking ? (
					<div className="flex flex-col items-center gap-3 py-12">
						<Spinner size="lg" />
						<p className="text-sm text-muted">
							{t("checkingSource")}
							<CheckingElapsed key={diffSource} />
						</p>
					</div>
				) : data?.needsCredential ? (
					<div className="space-y-4">
						<Alert status="warning">
							<Alert.Indicator />
							<Alert.Content>
								<Alert.Title>
									{t("needsCredential")}
								</Alert.Title>
								<Alert.Description>
									{t("needsCredentialHint")}
								</Alert.Description>
								<div className="mt-3">
									<Button
										size="sm"
										variant="secondary"
										onPress={() =>
											setIsCredentialDialogOpen(true)
										}
									>
										{t("credentialBind")}
									</Button>
								</div>
							</Alert.Content>
						</Alert>
						{/* Dialog is mounted once at root level — just trigger open above */}
					</div>
				) : data?.uncheckableReason ? (
					// Nothing was COMPARED. Without this the same response —
					// an empty skill list — renders as "this source is fine".
					// A persistent cached-data warning, so it needs the live
					// region: it replaces a spinner without moving focus, and a
					// screen reader would otherwise never hear that the rows are
					// not current (desktop AGENTS.md).
					<Alert status="warning" role="alert" aria-live="polite">
						<Alert.Indicator />
						<Alert.Content>
							<Alert.Title>{t("sourceUncheckable")}</Alert.Title>
							<Alert.Description>
								{t("sourceUncheckableHint", {
									reason: data.uncheckableReason,
								})}
							</Alert.Description>
						</Alert.Content>
					</Alert>
				) : (
					<div className="space-y-6">
						{/* Summary bar — per-source counts */}
						<SummaryBar
							notInstalledCount={notInstalled.length}
							outdatedCount={outdated.length}
							renamedCount={renamed.length}
							uncheckableCount={uncheckable.length}
							currentCount={current.length}
							isLoading={isLoading}
						/>

						{hasVisibleSkills && orphanLockCount > 0 && (
							<SourceOrphanLockAlert
								prunedCount={orphanLockCount}
								isChecking={prunePreviewQuery.isFetching}
								isCleaning={pruneLockMutation.isPending}
								onClean={() => pruneLockMutation.mutate()}
							/>
						)}
						{!hasVisibleSkills && (
							<SourceEmptyState
								prunedCount={orphanLockCount}
								isChecking={prunePreviewQuery.isFetching}
								isCleaning={pruneLockMutation.isPending}
								hasError={prunePreviewQuery.isError}
								onClean={() => pruneLockMutation.mutate()}
								onRetry={() => prunePreviewQuery.refetch()}
							/>
						)}

						{/* "Needs action" card — all actionable states mixed */}
						{hasNeedsAction && (
							<section>
								<div className="mb-2 flex flex-wrap items-center justify-between gap-x-3 gap-y-2">
									<div className="flex min-w-0 items-center gap-2">
										<ExclamationTriangleIcon className="size-4 shrink-0 text-warning" />
										<h2 className="truncate text-sm font-semibold text-foreground">
											{t("sourceNeedsAction")}
										</h2>
										<span className="shrink-0 text-xs text-muted">
											{needsActionSkills.length}
										</span>
									</div>
									{/* Batch buttons */}
									<div className="flex flex-wrap items-center justify-end gap-1">
										{outdated.length > 0 && (
											<Button
												size="sm"
												variant="ghost"
												className="h-7 px-2 text-xs"
												isDisabled={
													isApplyingAll ||
													isDeletingAllRemoved ||
													applyUpdateMutation.isPending
												}
												onPress={() =>
													applyAllUpdates(outdated)
												}
											>
												<ArrowPathIcon className="size-3.5" />
												{isApplyingAll
													? t("sourceUpdating")
													: t("sourceUpdateAll")}
											</Button>
										)}
										{notInstalled.length > 0 && (
											<>
												<button
													type="button"
													className="h-7 rounded px-2 text-xs text-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
													disabled={
														isInstallingAll ||
														installingSkillPath !==
															null
													}
													onClick={() =>
														setSelectedInstallSkillPaths(
															allInstallSkillsSelected
																? new Set()
																: new Set(
																		allInstallSkillPaths,
																	),
														)
													}
												>
													{allInstallSkillsSelected
														? t(
																"sourceClearSelection",
															)
														: t("sourceSelectAll")}
												</button>
												<Button
													size="sm"
													variant="ghost"
													className="h-7 px-2 text-xs"
													isDisabled={
														isInstallingAll ||
														installingSkillPath !==
															null ||
														isCoverageLoading
													}
													onPress={() =>
														installFromSource(
															hasSelectedInstallSkills
																? selectedInstallSkills
																: notInstalled,
														)
													}
												>
													<ArrowDownTrayIcon className="size-3.5" />
													{isInstallingAll
														? t("sourceInstalling")
														: hasSelectedInstallSkills
															? t(
																	"sourceInstallSelected",
																	{
																		count: selectedInstallCount,
																	},
																)
															: t(
																	"sourceInstallAll",
																)}
												</Button>
											</>
										)}
										{removed.length > 0 && (
											<Button
												size="sm"
												variant="ghost"
												className="h-7 px-2 text-xs"
												isDisabled={
													isApplyingAll ||
													isDeletingAllRemoved ||
													deleteRemovedSkillMutation.isPending
												}
												onPress={() =>
													deleteAllRemovedSkills(
														removed,
													)
												}
											>
												<TrashIcon className="size-3.5" />
												{isDeletingAllRemoved
													? `${t("sourceRemovedCleaning")} ${batchDone}/${removed.length}`
													: t(
															"sourceRemovedCleanAll",
														)}
											</Button>
										)}
									</div>
								</div>

								<ul className="overflow-hidden rounded-lg border border-border">
									{outdated.map((skill) => {
										const isApplying =
											applyUpdateMutation.isPending &&
											applyUpdateMutation.variables?.body
												.name === skill.name;
										return (
											<SourceSkillRow
												key={skill.skillPath}
												skill={skill}
												isExpanded={
													expandedSkillPath ===
													skill.skillPath
												}
												onToggle={() =>
													setExpandedSkillPath(
														expandedSkillPath ===
															skill.skillPath
															? null
															: skill.skillPath,
													)
												}
												action={
													<Button
														size="sm"
														variant="secondary"
														className="h-7 px-2 text-xs"
														isDisabled={
															isApplyingAll ||
															isApplying
														}
														onPress={() =>
															applyOneUpdate(
																skill,
															)
														}
													>
														<ArrowPathIcon className="size-3.5" />
														{isApplying
															? t(
																	"sourceUpdating",
																)
															: t(
																	"sourceUpdateSkill",
																)}
													</Button>
												}
											/>
										);
									})}
									{notInstalled.map((skill) => {
										const isInstalling =
											installingSkillPath ===
											skill.skillPath;
										return (
											<SourceSkillRow
												key={skill.skillPath}
												skill={skill}
												isExpanded={
													expandedSkillPath ===
													skill.skillPath
												}
												onToggle={() =>
													setExpandedSkillPath(
														expandedSkillPath ===
															skill.skillPath
															? null
															: skill.skillPath,
													)
												}
												isSelected={selectedInstallSkillPaths.has(
													skill.skillPath,
												)}
												onToggleSelected={() =>
													toggleInstallSkillSelection(
														skill,
													)
												}
												isSelectionDisabled={
													isInstallingAll ||
													installingSkillPath !== null
												}
												action={
													<Button
														size="sm"
														variant="secondary"
														className="h-7 px-2 text-xs"
														isDisabled={
															isInstallingAll ||
															installingSkillPath !==
																null ||
															isCoverageLoading
														}
														onPress={() =>
															installFromSource([
																skill,
															])
														}
													>
														<ArrowDownTrayIcon className="size-3.5" />
														{isInstalling
															? t(
																	"sourceInstalling",
																)
															: t(
																	"sourceInstallSkill",
																)}
													</Button>
												}
											/>
										);
									})}
									{/* TODO(Phase 3): replace with one-click accept-rename once
									    POST /skills/accept-rename exists. For now, keep the
									    existing two-step flow: delete old + copy install cmd. */}
									{renamed.map((skill) => {
										const isDeleting =
											deleteRenamedSkillMutation.isPending &&
											deleteRenamedSkillMutation.variables
												?.skillPath === skill.skillPath;
										const isCopying =
											copyRenamedInstallMutation.isPending &&
											copyRenamedInstallMutation.variables
												?.skillPath === skill.skillPath;
										const rowBusy = isDeleting || isCopying;
										return (
											<SourceSkillRow
												key={skill.skillPath}
												skill={skill}
												isExpanded={
													expandedSkillPath ===
													skill.skillPath
												}
												onToggle={() =>
													setExpandedSkillPath(
														expandedSkillPath ===
															skill.skillPath
															? null
															: skill.skillPath,
													)
												}
												showReason
												action={
													<div className="flex items-center gap-1.5">
														<Button
															size="sm"
															variant="secondary"
															className="h-7 px-2 text-xs"
															isDisabled={
																!skill.previousName ||
																rowBusy
															}
															onPress={() =>
																deleteRenamedSkillMutation.mutate(
																	skill,
																)
															}
														>
															<TrashIcon className="size-3.5" />
															{isDeleting
																? t(
																		"sourceRenamedDeleting",
																	)
																: t(
																		"sourceRenamedDeleteOld",
																	)}
														</Button>
														<Button
															size="sm"
															variant="ghost"
															className="h-7 px-2 text-xs"
															isDisabled={rowBusy}
															onPress={() =>
																copyRenamedInstallMutation.mutate(
																	skill,
																)
															}
														>
															<ClipboardDocumentIcon className="size-3.5" />
															{isCopying
																? t(
																		"sourceRenamedCopying",
																	)
																: t(
																		"sourceRenamedCopyInstall",
																	)}
														</Button>
													</div>
												}
											/>
										);
									})}
									{removed.map((skill) => {
										const isDeleting =
											deleteRemovedSkillMutation.isPending &&
											deleteRemovedSkillMutation.variables
												?.skillPath === skill.skillPath;
										return (
											<SourceSkillRow
												key={skill.skillPath}
												skill={skill}
												isExpanded={
													expandedSkillPath ===
													skill.skillPath
												}
												onToggle={() =>
													setExpandedSkillPath(
														expandedSkillPath ===
															skill.skillPath
															? null
															: skill.skillPath,
													)
												}
												muted
												showReason
												action={
													<Button
														size="sm"
														variant="secondary"
														className="h-7 px-2 text-xs"
														isDisabled={
															isDeletingAllRemoved ||
															isDeleting
														}
														onPress={() =>
															deleteRemovedSkillMutation.mutate(
																skill,
															)
														}
													>
														<TrashIcon className="size-3.5" />
														{isDeleting
															? t(
																	"sourceRemovedCleaning",
																)
															: t(
																	"sourceRemovedCleanSkill",
																)}
													</Button>
												}
											/>
										);
									})}
									{installedDeprecated.map((skill) => {
										const isDeleting =
											deleteRemovedSkillMutation.isPending &&
											deleteRemovedSkillMutation.variables
												?.skillPath === skill.skillPath;
										return (
											<SourceSkillRow
												key={skill.skillPath}
												skill={skill}
												isExpanded={
													expandedSkillPath ===
													skill.skillPath
												}
												onToggle={() =>
													setExpandedSkillPath(
														expandedSkillPath ===
															skill.skillPath
															? null
															: skill.skillPath,
													)
												}
												muted
												showReason
												action={
													<Button
														size="sm"
														variant="secondary"
														className="h-7 px-2 text-xs"
														isDisabled={
															isDeletingAllRemoved ||
															isDeleting
														}
														onPress={() =>
															deleteRemovedSkillMutation.mutate(
																skill,
															)
														}
													>
														<TrashIcon className="size-3.5" />
														{isDeleting
															? t(
																	"sourceRemovedCleaning",
																)
															: t(
																	"sourceRemovedCleanSkill",
																)}
													</Button>
												}
											/>
										);
									})}
									{/* Only auth-blocked uncheckable rows are
									    actionable (credential binding). Non-auth
									    uncheckable rows are rendered below as
									    an info note, not action rows. */}
									{uncheckableAuth.map((skill) => (
										<SourceSkillRow
											key={skill.skillPath}
											skill={skill}
											isExpanded={
												expandedSkillPath ===
												skill.skillPath
											}
											onToggle={() =>
												setExpandedSkillPath(
													expandedSkillPath ===
														skill.skillPath
														? null
														: skill.skillPath,
												)
											}
											muted
											showReason
											action={
												<Button
													size="sm"
													variant="secondary"
													className="h-7 px-2 text-xs"
													onPress={() =>
														setIsCredentialDialogOpen(
															true,
														)
													}
												>
													{t("credentialBind")}
												</Button>
											}
										/>
									))}
								</ul>
							</section>
						)}

						{/* Non-auth uncheckable: info-only, no action */}
						{uncheckableNonAuth.length > 0 && (
							<SkillSection
								title={t("summaryUnchecked", {
									count: uncheckableNonAuth.length,
								})}
								icon={
									<ExclamationTriangleIcon className="size-4 text-muted" />
								}
								skills={uncheckableNonAuth}
								expandedSkillPath={expandedSkillPath}
								onToggleSkill={setExpandedSkillPath}
								muted
							/>
						)}

						{/* "Installed (latest)" — collapsed by default */}
						<SkillSection
							title={t("sourceStateCurrent")}
							icon={
								<CheckCircleIcon className="size-4 text-success" />
							}
							skills={current}
							expandedSkillPath={expandedSkillPath}
							onToggleSkill={setExpandedSkillPath}
							defaultCollapsed
						/>

						{/* All-clear empty state — only when truly NO non-current rows exist.
						    Non-auth uncheckable rows are excluded from hasNeedsAction, so
						    we must also guard uncheckableNonAuth to avoid a false "all up to
						    date" while the same panel shows unchecked rows. */}
						{!hasNeedsAction &&
							uncheckableNonAuth.length === 0 &&
							!isLoading &&
							current.length > 0 && (
								<div className="rounded-lg border border-success/30 bg-success/5 px-4 py-3">
									<div className="flex items-center gap-2">
										<CheckCircleIcon className="size-4 shrink-0 text-success" />
										<p className="text-sm text-success">
											{t("sourceAllLatest")}
										</p>
									</div>
								</div>
							)}

						{/* Agent coverage hint. The old version counted an
						 * "already covered" bucket beside the link targets —
						 * agents that received the skill whether or not the
						 * user picked them. That bucket is empty by
						 * construction now; what is worth surfacing instead is
						 * which agents cannot be chosen apart. */}
						{linkTargets.length > 0 && notInstalled.length > 0 && (
							<div className="flex flex-wrap items-center gap-1.5 text-xs text-muted">
								<span>
									{linkTargets.length}{" "}
									{t("sourceInstallLinkTargetsTitle")}
								</span>
								{sharedGroups.length > 0 && (
									<>
										<span className="mx-1 text-muted/50">
											·
										</span>
										{sharedGroups.map((group) => (
											<Chip
												key={group.members
													.map((a) => a.id)
													.join("+")}
												size="sm"
												variant="secondary"
											>
												{group.members
													.map((a) => a.display_name)
													.join(" + ")}
											</Chip>
										))}
									</>
								)}
							</div>
						)}
					</div>
				)}
			</div>

			{/* Credential dialog also mounts at root level for uncheckable rows */}
			<SourceCredentialBindingDialog
				isOpen={isCredentialDialogOpen}
				bindingSource={credentialBindingSource}
				onClose={() => setIsCredentialDialogOpen(false)}
				onBound={async () => {
					setIsCredentialDialogOpen(false);
					await queryClient.invalidateQueries({
						queryKey: queryKeys.skills.sources.all(),
					});
				}}
			/>
		</div>
	);
}
