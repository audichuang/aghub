import { AlertDialog, Button, Checkbox, Modal, toast } from "@heroui/react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { useApi } from "../hooks/use-api";
import { supportsSkillMutation } from "../lib/agent-capabilities";
import {
	buildReconcilePlans,
	computeGroupAgentStats,
} from "../lib/group-agent-plan";
import { cn } from "../lib/utils";
import { reconcileSkillsMutationOptions } from "../requests/skills";

interface BulkManageGroupAgentsDialogProps {
	source: string;
	skills: { name: string; items: { agent: string; source: string }[] }[];
	scope: "global" | "project";
	projectPath?: string;
	isOpen: boolean;
	onClose: () => void;
}

export function BulkManageGroupAgentsDialog({
	source,
	skills,
	scope,
	projectPath,
	isOpen,
	onClose,
}: BulkManageGroupAgentsDialogProps) {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const { availableAgents } = useAgentAvailability();
	const reconcileMutation = useMutation(
		reconcileSkillsMutationOptions({ api, queryClient }),
	);

	// Agents that can mutate skills at this scope. Their display names are
	// keyed for the rows.
	const usableAgents = useMemo(
		() =>
			(availableAgents ?? []).filter(
				(a) => a?.isUsable && supportsSkillMutation(a, scope),
			),
		[availableAgents, scope],
	);
	const usableAgentIds = useMemo(
		() => usableAgents.map((a) => a.id),
		[usableAgents],
	);

	const stats = useMemo(
		() => computeGroupAgentStats(skills, usableAgentIds),
		[skills, usableAgentIds],
	);
	const statById = useMemo(
		() => new Map(stats.map((s) => [s.agentId, s])),
		[stats],
	);

	// Only agents the user explicitly toggles ("touched") participate in the
	// reconcile — untouched agents (including "some"/"none") are never added or
	// removed. This is the delete-protection model: a removal happens ONLY when
	// the user actively unchecks an agent.
	const [desired, setDesired] = useState<Record<string, boolean>>({});
	const [isApplying, setIsApplying] = useState(false);
	const [done, setDone] = useState(0);
	const [confirmRemoveOpen, setConfirmRemoveOpen] = useState(false);

	const plans = useMemo(() => {
		const touchedIds = Object.keys(desired);
		const desiredSet = new Set(touchedIds.filter((id) => desired[id]));
		return buildReconcilePlans(skills, touchedIds, desiredSet);
	}, [skills, desired]);

	const removeCount = useMemo(
		() => plans.filter((p) => p.removed.length > 0).length,
		[plans],
	);
	const hasChanges = plans.length > 0;

	const reset = () => {
		setDesired({});
		setIsApplying(false);
		setDone(0);
		setConfirmRemoveOpen(false);
	};

	const handleClose = () => {
		reset();
		onClose();
	};

	const rowState = (agentId: string) => {
		if (agentId in desired) {
			return { isSelected: desired[agentId], isIndeterminate: false };
		}
		const state = statById.get(agentId)?.state ?? "none";
		return {
			isSelected: state === "all",
			isIndeterminate: state === "some",
		};
	};

	const runApply = async () => {
		setConfirmRemoveOpen(false);
		setIsApplying(true);
		setDone(0);
		let success = 0;
		let failed = 0;
		try {
			for (const plan of plans) {
				try {
					const result = await reconcileMutation.mutateAsync({
						source: {
							agent: plan.sourceAgent,
							scope: plan.scope,
							project_root: projectPath ?? null,
							name: plan.name,
						},
						added: plan.added.length > 0 ? plan.added : null,
						removed: plan.removed.length > 0 ? plan.removed : null,
					});
					if (result.failed_count === 0) {
						success += 1;
					} else {
						failed += 1;
					}
				} catch {
					failed += 1;
				}
				setDone(success + failed);
			}
			if (failed === 0) {
				toast.success(t("bulkAgentsDone", { count: success }));
			} else {
				toast.danger(t("bulkAgentsSomeFailed", { success, failed }));
			}
			handleClose();
		} finally {
			setIsApplying(false);
		}
	};

	const handleApply = () => {
		if (!hasChanges) return;
		if (removeCount > 0) {
			setConfirmRemoveOpen(true);
			return;
		}
		void runApply();
	};

	const confirmLabel = isApplying
		? t("bulkAgentsApplying", { done, total: plans.length })
		: t("bulkAgentsApply");

	return (
		<>
			<Modal.Backdrop isOpen={isOpen} onOpenChange={handleClose}>
				<Modal.Container>
					<Modal.Dialog className="w-[calc(100vw-2rem)] max-w-md sm:max-w-lg">
						<Modal.CloseTrigger />
						<Modal.Header>
							<Modal.Heading>
								{t("bulkManageGroupAgents")}
							</Modal.Heading>
						</Modal.Header>

						<Modal.Body className="p-4">
							<p className="mb-3 text-sm text-muted">
								{t("bulkManageGroupAgentsHint", {
									source,
									count: skills.length,
								})}
							</p>
							<div
								className={cn(
									"space-y-1 transition-opacity",
									isApplying && "opacity-50",
								)}
							>
								{usableAgents.map((agent) => {
									const stat = statById.get(agent.id);
									const { isSelected, isIndeterminate } =
										rowState(agent.id);
									return (
										<Checkbox
											key={agent.id}
											variant="secondary"
											className="flex w-full items-center justify-between gap-3 rounded-lg px-2 py-1.5 hover:bg-surface-secondary"
											isSelected={isSelected}
											isIndeterminate={isIndeterminate}
											isDisabled={isApplying}
											onChange={(next) =>
												setDesired((prev) => ({
													...prev,
													[agent.id]: next,
												}))
											}
										>
											<Checkbox.Control>
												<Checkbox.Indicator />
											</Checkbox.Control>
											<Checkbox.Content className="flex flex-1 items-center justify-between gap-2">
												<span className="text-sm text-foreground">
													{agent.display_name}
												</span>
												<span className="text-xs text-muted">
													{stat?.installed ?? 0}/
													{stat?.total ??
														skills.length}
												</span>
											</Checkbox.Content>
										</Checkbox>
									);
								})}
								{usableAgents.length === 0 && (
									<p className="text-sm text-muted">
										{t("noTargetAgents")}
									</p>
								)}
							</div>
						</Modal.Body>

						<Modal.Footer>
							<Button
								slot="close"
								variant="secondary"
								isDisabled={isApplying}
							>
								{t("cancel")}
							</Button>
							<Button
								onPress={handleApply}
								isDisabled={!hasChanges || isApplying}
							>
								{confirmLabel}
							</Button>
						</Modal.Footer>
					</Modal.Dialog>
				</Modal.Container>
			</Modal.Backdrop>

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
								{t("bulkAgentsConfirmRemoveBody", {
									count: removeCount,
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
								{t("bulkAgentsApply")}
							</Button>
						</AlertDialog.Footer>
					</AlertDialog.Dialog>
				</AlertDialog.Container>
			</AlertDialog.Backdrop>
		</>
	);
}
