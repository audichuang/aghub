import {
	ExclamationTriangleIcon,
	XCircleIcon,
} from "@heroicons/react/24/solid";
import {
	AlertDialog,
	Button,
	Checkbox,
	Modal,
	Spinner,
	toast,
} from "@heroui/react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import * as pathe from "pathe";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { SkillResponse } from "../generated/dto";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { useAgentName } from "../hooks/use-agent-name";
import { useApi } from "../hooks/use-api";
import { keptDeleteMessage } from "../lib/skill-delete-message";
import { splitDeleteTargets } from "../lib/skill-delete-targets";
import {
	failedReconcileRowsMessage,
	isWholeBatchRefusal,
} from "../lib/skill-reconcile-errors";
import { invalidateSkillQueries } from "../requests/skills";
import type { LocationGroup, SkillGroup } from "./skill-detail-helpers";

interface DeleteSkillLocationDialogProps {
	item: LocationGroup | null;
	isOpen: boolean;
	onClose: () => void;
	projectPath?: string;
	skillName: string;
}

interface DeleteSkillDialogProps {
	group: SkillGroup;
	isOpen: boolean;
	onClose: () => void;
	projectPath?: string;
}

export function DeleteSkillLocationDialog({
	item,
	isOpen,
	onClose,
	projectPath,
	skillName,
}: DeleteSkillLocationDialogProps) {
	const { t } = useTranslation();
	const agentName = useAgentName();
	const api = useApi();
	const queryClient = useQueryClient();
	const deleteRequest =
		item && item.installations.length > 0
			? {
					source_path: item.sourcePath,
					// The by-path route gates on confirm (unwrap_or(false) =>
					// dry-run returning success:true), so without this the
					// delete silently no-ops yet the success check passes.
					confirm: true,
					agents: item.installations.map(
						(installation) => installation.agent,
					),
					scope:
						item.installations[0].source === "project"
							? ("project" as const)
							: ("global" as const),
					project_root:
						item.installations[0].source === "project"
							? (projectPath ?? null)
							: null,
				}
			: null;

	const deleteMutation = useMutation({
		mutationFn: async () => {
			if (!deleteRequest) {
				return;
			}

			const result = await api.skills.deleteByPath(deleteRequest);

			if (result.outcome === "kept") {
				// The `.agents/skills` master is shared and another agent still
				// reads it, so NOTHING was removed. `success` is true here (the
				// request was understood), and reading only that closed this
				// dialog and refreshed the list as if the skill were gone —
				// while it was still installed and still visible.
				//
				// Never `result.error ||` here: that is English, set only by a
				// git refusal, and it overrode the localized text.
				throw new Error(keptDeleteMessage(result, skillName, t));
			}
			if (result.outcome === "partial") {
				// Some paths went, some did not. Still an error for the user —
				// the skill is not gone — but the list MUST be refreshed,
				// because part of it really was removed. Throwing without
				// invalidating would leave stale entries on screen.
				await invalidateSkillQueries(queryClient);
				throw new Error(t("deleteSkillPartial", { name: skillName }));
			}
			if (result.outcome !== "removed" && result.outcome !== "absent") {
				// `absent` is a success for a delete: the post-condition
				// ("the skill is gone") already holds.
				throw new Error(result.error || t("failedToDeleteSkill"));
			}
		},
		onSuccess: async () => {
			await invalidateSkillQueries(queryClient);
			onClose();
		},
		onError: (error) => {
			console.error("Skill location delete mutation error:", error);
			toast.danger(
				error instanceof Error
					? error.message
					: t("failedToDeleteSkill"),
			);
		},
	});

	const folderPath = item ? pathe.dirname(item.sourcePath) : "";
	const agentNames =
		item?.installations.length === 1
			? agentName(item.installations[0].agent)
			: (item?.installations.map((i) => agentName(i.agent)).join(", ") ??
				"");
	const isMultiAgent = (item?.installations.length ?? 0) > 1;

	return (
		<AlertDialog.Backdrop isOpen={isOpen} onOpenChange={onClose}>
			<AlertDialog.Container>
				<AlertDialog.Dialog className="sm:max-w-[420px]">
					<AlertDialog.CloseTrigger />
					<AlertDialog.Header>
						<AlertDialog.Icon status="danger" />
						<AlertDialog.Heading>
							{isMultiAgent
								? t("deleteSkillTitle")
								: t("deleteSkillForAgentTitle", {
										agent: agentNames,
									})}
						</AlertDialog.Heading>
					</AlertDialog.Header>
					<AlertDialog.Body>
						<p className="text-sm text-muted">
							{isMultiAgent
								? t("deleteSkillForAgentsWarning", {
										name: skillName,
										agents: agentNames,
									})
								: t("deleteSkillForAgentWarning", {
										name: skillName,
										agent: agentNames,
									})}
						</p>
						{item && (
							<div className="mt-4 rounded-lg bg-surface-secondary px-3 py-2">
								<p className="text-[11px] text-muted">
									{isMultiAgent
										? t("sharedLocation")
										: item.installations[0].source ===
											  "project"
											? t("project")
											: t("global")}
								</p>
								<p className="mt-1 font-mono text-xs text-foreground">
									{folderPath}
								</p>
							</div>
						)}
					</AlertDialog.Body>
					<AlertDialog.Footer>
						<Button
							slot="close"
							variant="tertiary"
							onPress={onClose}
							isDisabled={deleteMutation.isPending}
						>
							{t("cancel")}
						</Button>
						<Button
							variant="danger"
							onPress={() => deleteMutation.mutate()}
							isDisabled={deleteMutation.isPending}
						>
							{deleteMutation.isPending ? (
								<>
									<Spinner
										size="sm"
										color="current"
										className="mr-2"
									/>
									{t("deleting")}
								</>
							) : (
								t("delete")
							)}
						</Button>
					</AlertDialog.Footer>
				</AlertDialog.Dialog>
			</AlertDialog.Container>
		</AlertDialog.Backdrop>
	);
}

export function DeleteSkillDialog({
	group,
	isOpen,
	onClose,
	projectPath,
}: DeleteSkillDialogProps) {
	const { t } = useTranslation();
	const agentName = useAgentName();
	const api = useApi();
	const queryClient = useQueryClient();

	const skill = group.items[0];
	const { availableAgents } = useAgentAvailability();
	// Agents the user turned off still read the skill and keep its shared
	// Master alive, so they are listed apart and only named on request.
	// "Managed" is core's `agent_settings::is_managed` (not disabled), NOT
	// `isUsable`: an enabled agent that is merely undetected is still a reader
	// the server counts, and leaving it unnamed made the request refuse itself.
	const [includeUnmanaged, setIncludeUnmanaged] = useState(false);
	const managedAgentIds = useMemo(
		() =>
			new Set(
				availableAgents
					.filter((agent) => !agent.isDisabled)
					.map((agent) => agent.id),
			),
		[availableAgents],
	);
	const targets = useMemo(
		() =>
			splitDeleteTargets(group.items, managedAgentIds, includeUnmanaged),
		[group.items, managedAgentIds, includeUnmanaged],
	);

	const deleteMutation = useMutation({
		mutationFn: async () => {
			const itemsWithAgent = targets.named.filter(
				(item): item is SkillResponse & { agent: string } =>
					!!item.agent,
			);

			const globalItems = itemsWithAgent.filter(
				(item) => item.source === "global",
			);
			const projectItems = itemsWithAgent.filter(
				(item) => item.source === "project",
			);

			const results = [];

			if (globalItems.length > 0) {
				const result = await api.skills.reconcile({
					source: {
						agent: globalItems[0].agent,
						scope: "global",
						project_root: null,
						name: skill.name,
					},
					added: null,
					removed: globalItems.map((item) => item.agent),
					// This dialog is the "remove from these agents" confirmation.
					confirm: true,
				});
				results.push(result);
			}

			if (projectItems.length > 0) {
				const result = await api.skills.reconcile({
					source: {
						agent: projectItems[0].agent,
						scope: "project",
						project_root: projectPath ?? null,
						name: skill.name,
					},
					added: null,
					removed: projectItems.map((item) => item.agent),
					// This dialog is the "remove from these agents" confirmation.
					confirm: true,
				});
				results.push(result);
			}

			const totalFailed = results.reduce(
				(sum, r) => sum + r.failed_count,
				0,
			);
			const totalResults = results.reduce(
				(sum, r) => sum + r.results.length,
				0,
			);

			if (totalFailed > 0) {
				const allRows = results.flatMap((r) => r.results);
				const message =
					failedReconcileRowsMessage(allRows, agentName) ??
					`${totalFailed} of ${totalResults} deletions failed`;
				throw new Error(message);
			}
		},
		onSuccess: async () => {
			// A link the user did not ask us to touch may still keep the skill on
			// disk — but it may not: a disabled agent's link in a shared slot goes
			// once every enabled reader of that slot is named. Ask disk, not the
			// request, before claiming the skill was kept.
			if (targets.unmanaged.length > 0 && !includeUnmanaged) {
				const unmanaged = new Set(
					targets.unmanaged.map((item) => item.agent),
				);
				const scopes = new Set(
					targets.unmanaged.map((item) => item.source),
				);
				try {
					const lists = await Promise.all(
						[...scopes].map((scope) =>
							scope === "project"
								? api.skills.listAll("project", projectPath)
								: api.skills.listAll("global"),
						),
					);
					if (
						lists
							.flat()
							.some(
								(item) =>
									item.name === skill.name &&
									unmanaged.has(item.agent),
							)
					) {
						toast.info(t("deleteSkillKeptForUnmanaged"));
					}
				} catch {
					// Cannot tell; say nothing rather than guess.
				}
			}
			await invalidateSkillQueries(queryClient);
			setIncludeUnmanaged(false);
			onClose();
		},
		onError: async (error) => {
			console.error("Skill delete mutation error:", error);
			// An unticked unmanaged holder whose link sits in a slot the named
			// agents read makes the server refuse the whole batch; the only way
			// forward is the checkbox, so say that and keep the dialog open.
			toast.danger(
				isWholeBatchRefusal(error) &&
					targets.unmanaged.length > 0 &&
					!includeUnmanaged
					? t("deleteSkillRetryWithUnmanaged")
					: error instanceof Error
						? error.message
						: t("failedToDeleteSkill"),
			);
			await invalidateSkillQueries(queryClient);
		},
	});

	const globalItems = targets.managed.filter(
		(item) => item.source === "global",
	);
	const projectItems = targets.managed.filter(
		(item) => item.source === "project",
	);

	return (
		<Modal.Backdrop isOpen={isOpen} onOpenChange={onClose}>
			<Modal.Container>
				<Modal.Dialog>
					<Modal.CloseTrigger />
					<Modal.Header>
						<div className="flex items-center gap-2">
							<ExclamationTriangleIcon className="size-5 text-warning" />
							<Modal.Heading>{t("deleteSkill")}</Modal.Heading>
						</div>
					</Modal.Header>

					<Modal.Body className="p-2">
						<p className="mb-4 text-sm text-muted">
							{t("deleteSkillWarning", {
								count: targets.named.length,
							})}
						</p>

						<div className="space-y-4">
							{globalItems.length > 0 && (
								<div>
									<h4
										className="
											mb-2 text-xs font-medium tracking-wide text-muted
											uppercase
										"
									>
										{t("globalSkills")}
									</h4>
									<div className="space-y-2">
										{globalItems.map((item) => (
											<div
												key={item.agent}
												className="flex items-center gap-2 text-sm"
											>
												<XCircleIcon className="size-4 shrink-0 text-danger" />
												<span className="text-foreground">
													{item.agent
														? agentName(item.agent)
														: t("default")}
												</span>
												{item.source_path && (
													<span className="flex-1 truncate text-xs text-muted">
														{item.source_path}
													</span>
												)}
											</div>
										))}
									</div>
								</div>
							)}

							{projectItems.length > 0 && (
								<div>
									<h4
										className="
											mb-2 text-xs font-medium tracking-wide text-muted
											uppercase
										"
									>
										{t("projectSkills")}
									</h4>
									<div className="space-y-2">
										{projectItems.map((item) => (
											<div
												key={item.agent}
												className="flex items-center gap-2 text-sm"
											>
												<XCircleIcon className="size-4 shrink-0 text-danger" />
												<span className="text-foreground">
													{item.agent
														? agentName(item.agent)
														: t("default")}
												</span>
												{item.source_path && (
													<span className="flex-1 truncate text-xs text-muted">
														{item.source_path}
													</span>
												)}
											</div>
										))}
									</div>
								</div>
							)}
						</div>

						{targets.unmanaged.length > 0 && (
							<div className="mt-4 rounded-lg bg-surface-secondary p-3">
								<h4
									className="
										mb-1 text-xs font-medium tracking-wide text-muted
										uppercase
									"
								>
									{t("deleteSkillUnmanagedTitle")}
								</h4>
								<p className="mb-2 text-xs text-muted">
									{t("deleteSkillUnmanagedHint")}
								</p>
								<div className="mb-3 space-y-1">
									{targets.unmanaged.map((item) => (
										<div
											key={`${item.source}:${item.agent}`}
											className="flex items-center gap-2 text-sm"
										>
											<span className="text-foreground">
												{item.agent
													? agentName(item.agent)
													: t("default")}
											</span>
											{item.source_path && (
												<span className="flex-1 truncate text-xs text-muted">
													{item.source_path}
												</span>
											)}
										</div>
									))}
								</div>
								<Checkbox
									isSelected={includeUnmanaged}
									onChange={setIncludeUnmanaged}
									isDisabled={deleteMutation.isPending}
								>
									<Checkbox.Content>
										<Checkbox.Control>
											<Checkbox.Indicator />
										</Checkbox.Control>
										<span className="text-sm">
											{t("deleteSkillIncludeUnmanaged")}
										</span>
									</Checkbox.Content>
								</Checkbox>
							</div>
						)}
					</Modal.Body>

					<Modal.Footer>
						<Button
							slot="close"
							variant="secondary"
							onPress={onClose}
							isDisabled={deleteMutation.isPending}
						>
							{t("cancel")}
						</Button>
						<Button
							variant="danger"
							onPress={() => deleteMutation.mutate()}
							isDisabled={
								deleteMutation.isPending ||
								targets.named.length === 0
							}
						>
							{deleteMutation.isPending ? (
								<>
									<Spinner
										size="sm"
										color="current"
										className="mr-2"
									/>
									{t("deleting")}
								</>
							) : (
								t("deleteAll")
							)}
						</Button>
					</Modal.Footer>
				</Modal.Dialog>
			</Modal.Container>
		</Modal.Backdrop>
	);
}
