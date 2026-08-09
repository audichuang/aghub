import { AlertDialog, Button, toast } from "@heroui/react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { useApi } from "../hooks/use-api";
import { supportsSkillMutation } from "../lib/agent-capabilities";
import type { InstallResult } from "../lib/install-utils";
import type { Scope } from "../lib/skills-path-group";
import {
	computeSkillAgentDiff,
	wouldOrphanSkill,
} from "../lib/group-agent-plan";
import { cn } from "../lib/utils";
import { reconcileSkillsMutationOptions } from "../requests/skills";
import type { AgentState } from "./agent-list";
import { SharedSkillInstallModal } from "./shared-skill-install-modal";
import type { SkillGroup } from "./skill-detail-helpers";
import { SkillsAgentList } from "./skills-agent-list";

interface ManageSkillAgentsDialogProps {
	group: SkillGroup;
	isOpen: boolean;
	onClose: () => void;
	projectPath?: string;
}

export function ManageSkillAgentsDialog({
	group,
	isOpen,
	onClose,
	projectPath,
}: ManageSkillAgentsDialogProps) {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const { availableAgents } = useAgentAvailability();
	const reconcileMutation = useMutation(
		reconcileSkillsMutationOptions({
			api,
			queryClient,
		}),
	);

	const hasValidGroup = group?.items && Array.isArray(group.items);

	const installedAgentIds = useMemo(() => {
		if (!hasValidGroup) return new Set<string>();
		return new Set(
			group.items
				.map((item) => item.agent)
				.filter((agent): agent is string => agent != null),
		);
	}, [hasValidGroup, group]);

	const scope: Scope = useMemo(() => {
		if (!hasValidGroup || group.items.length === 0) return "global";
		const primary = group.items[0];
		return primary?.source ?? "global";
	}, [hasValidGroup, group]);

	// Include installed agents so the user can uncheck them to remove — the
	// dialog manages both add AND remove. The reconcile API takes `added` +
	// `removed`, and core removal keeps shared masters intact. An installed
	// agent is listed even when it is not currently usable (e.g. its CLI went
	// away), otherwise there would be no way to remove the skill from it.
	const usableAgents = useMemo(
		() =>
			(availableAgents ?? []).filter(
				(a) =>
					a != null &&
					supportsSkillMutation(a, scope) &&
					(a.isUsable || installedAgentIds.has(a.id)),
			),
		[availableAgents, installedAgentIds, scope],
	);

	// "touched" overlay: agent id -> desired checked state. Untouched agents
	// fall back to their installed state, so re-opening resets cleanly without a
	// useEffect (reset() clears it on close).
	const [desired, setDesired] = useState<Record<string, boolean>>({});
	const [agentStates, setAgentStates] = useState<Record<string, AgentState>>(
		{},
	);
	const [isApplying, setIsApplying] = useState(false);
	const [confirmRemoveOpen, setConfirmRemoveOpen] = useState(false);

	const {
		selected: selectedAgents,
		added,
		removed,
		labels: diffLabels,
	} = useMemo(
		() =>
			computeSkillAgentDiff(
				usableAgents.map((a) => a.id),
				installedAgentIds,
				desired,
			),
		[usableAgents, installedAgentIds, desired],
	);

	const hasChanges = added.length > 0 || removed.length > 0;

	const handleSelectionChange = (keys: string[]) => {
		const keySet = new Set(keys);
		const before = new Set(selectedAgents);
		// Only record agents whose checked state actually flipped. Recording
		// every agent would freeze untouched ones against the live installed
		// state, so a background refetch that installs an untouched agent could
		// turn it into an unintended removal.
		setDesired((prev) => {
			const next = { ...prev };
			for (const agent of usableAgents) {
				const id = agent.id;
				if (before.has(id) !== keySet.has(id)) {
					next[id] = keySet.has(id);
				}
			}
			return next;
		});
	};

	const resetState = () => {
		setDesired({});
		setAgentStates({});
		setIsApplying(false);
		setConfirmRemoveOpen(false);
	};

	const onCloseAndReset = () => {
		resetState();
		onClose();
	};

	const runApply = async () => {
		setConfirmRemoveOpen(false);
		if (!hasValidGroup || group.items.length === 0) {
			toast.danger(t("invalidConfiguration"));
			return;
		}

		// Re-check the orphan guard at the single mutation choke-point: a
		// background refetch between opening the confirm dialog and pressing it
		// could have changed the plan, so the check in handleApply alone is
		// TOCTOU-unsafe.
		if (wouldOrphanSkill(installedAgentIds, added, removed)) {
			toast.danger(t("manageAgentsAddThenRemove"));
			return;
		}

		const primary = group.items[0];
		if (!primary?.name) {
			toast.danger(t("invalidSkillConfiguration"));
			return;
		}

		setIsApplying(true);
		const primaryAgent = primary.agent ?? "claude";
		const sourceAgentItem =
			group.items.find((item) => item.agent === primaryAgent) ?? primary;

		const touched = [...added, ...removed];
		const pendingStates: Record<string, AgentState> = {};
		for (const id of touched) {
			pendingStates[id] = { status: "pending" };
		}
		setAgentStates(pendingStates);

		try {
			const result = await reconcileMutation.mutateAsync({
				source: {
					agent: sourceAgentItem.agent ?? "claude",
					scope:
						sourceAgentItem.source === "project"
							? "project"
							: "global",
					project_root: projectPath ?? null,
					name: primary.name,
				},
				added: added.length > 0 ? added : null,
				removed: removed.length > 0 ? removed : null,
				// The user picked the agents to unlink in this dialog — that IS
				// the confirmation the API asks for before a removing reconcile.
				confirm: true,
			});

			const newAgentStates: Record<string, AgentState> = {};
			for (const item of result.results) {
				newAgentStates[item.agent] = {
					status: item.success ? "success" : "error",
					error: item.error ?? undefined,
				};
			}
			setAgentStates(newAgentStates);

			if (result.failed_count === 0) {
				toast.success(
					t("agentChangesApplied", { count: result.success_count }),
				);
				onCloseAndReset();
			} else {
				toast.danger(
					t("agentChangesFailed", {
						success: result.success_count,
						failed: result.failed_count,
					}),
				);
			}
		} catch (err) {
			const errorMessage =
				err instanceof Error ? err.message : t("unknownError");
			toast.danger(errorMessage);

			const errorStates: Record<string, AgentState> = {};
			for (const id of touched) {
				errorStates[id] = { status: "error", error: errorMessage };
			}
			setAgentStates(errorStates);
		} finally {
			setIsApplying(false);
		}
	};

	const handleApply = () => {
		if (!hasChanges) return;
		// Data-safety: "add to new agent(s) + remove every existing copy" in one
		// apply is unsafe — core copies before removing but does not abort the
		// removals if the copy fails, so a failed copy would leave the skill
		// installed nowhere. Make the user add first, then remove.
		if (wouldOrphanSkill(installedAgentIds, added, removed)) {
			toast.danger(t("manageAgentsAddThenRemove"));
			return;
		}
		// Removing is destructive — gate it behind an explicit confirm.
		if (removed.length > 0) {
			setConfirmRemoveOpen(true);
			return;
		}
		void runApply();
	};

	const agentPicker = !hasValidGroup ? (
		<p className="text-sm text-muted">{t("invalidConfiguration")}</p>
	) : (
		<div className={cn("transition-opacity", isApplying && "opacity-50")}>
			<SkillsAgentList
				agents={usableAgents}
				selectedKeys={selectedAgents}
				onSelectionChange={handleSelectionChange}
				scope={scope}
				agentStates={agentStates}
				diffLabels={diffLabels}
				disabled={isApplying}
				label={t("selectAgentsForSkill")}
				emptyMessage={t("noTargetAgents")}
			/>
		</div>
	);

	// ManageSkillAgentsDialog uses inline agentStates for per-row feedback;
	// it never enters the results phase of SharedSkillInstallModal.
	const NO_RESULTS: InstallResult[] = [];

	return (
		<>
			<SharedSkillInstallModal
				isOpen={isOpen}
				onClose={onCloseAndReset}
				heading={t("manageAgents")}
				agentPickerSlot={agentPicker}
				installResults={NO_RESULTS}
				isInstalling={isApplying}
				showTargetSelector={false}
				confirmLabel={isApplying ? t("applying") : t("applyChanges")}
				isConfirmDisabled={!hasChanges}
				onConfirm={handleApply}
			/>

			<AlertDialog.Backdrop
				isOpen={confirmRemoveOpen}
				onOpenChange={(open) => !open && setConfirmRemoveOpen(false)}
			>
				<AlertDialog.Container>
					<AlertDialog.Dialog className="sm:max-w-[420px]">
						<AlertDialog.CloseTrigger />
						<AlertDialog.Header>
							<AlertDialog.Icon status="danger" />
							<AlertDialog.Heading>
								{t("bulkAgentsConfirmRemoveTitle")}
							</AlertDialog.Heading>
						</AlertDialog.Header>
						<AlertDialog.Body>
							<p className="text-sm text-muted">
								{t("manageAgentsConfirmRemoveBody", {
									count: removed.length,
								})}
							</p>
						</AlertDialog.Body>
						<AlertDialog.Footer>
							<Button
								slot="close"
								variant="tertiary"
								onPress={() => setConfirmRemoveOpen(false)}
							>
								{t("cancel")}
							</Button>
							<Button
								variant="danger"
								onPress={() => void runApply()}
							>
								{t("applyChanges")}
							</Button>
						</AlertDialog.Footer>
					</AlertDialog.Dialog>
				</AlertDialog.Container>
			</AlertDialog.Backdrop>
		</>
	);
}
