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
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { SkillResponse } from "../generated/dto";
import { useAgentName } from "../hooks/use-agent-name";
import { useApi } from "../hooks/use-api";
import {
	deleteSkill,
	getUnmanagedAgents,
	splitDeleteTargets,
	type BackendHolders,
} from "../requests/delete-skill";
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
	const deleteMutation = useMutation({
		mutationFn: async () => {
			if (!item || item.installations.length === 0) {
				return;
			}
			const scope =
				item.installations[0].source === "project"
					? "project"
					: "global";
			const projectRoot =
				scope === "project" ? (projectPath ?? null) : null;
			const res = await deleteSkill({
				api,
				queryClient,
				skillName,
				t,
				intent: {
					kind: "by-path",
					sourcePath: item.sourcePath,
					scope,
					projectRoot,
					agents: item.installations.map((i) => i.agent),
				},
			});

			if (!res.success) {
				throw new Error(res.message || t("failedToDeleteSkill"));
			}
			return res;
		},
		onSuccess: () => {
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
	const [includeUnmanaged, setIncludeUnmanaged] = useState(false);
	const [backendHolders, setBackendHolders] = useState<BackendHolders | null>(
		null,
	);
	const [loadingHolders, setLoadingHolders] = useState(false);
	const [holdersError, setHoldersError] = useState<string | null>(null);

	useEffect(() => {
		if (!isOpen || !skill) {
			setBackendHolders(null);
			setHoldersError(null);
			setLoadingHolders(false);
			return;
		}
		setLoadingHolders(true);
		setHoldersError(null);
		getUnmanagedAgents(api)
			.then((holders) => {
				setBackendHolders(holders);
				setLoadingHolders(false);
			})
			.catch((err) => {
				setHoldersError(
					err instanceof Error ? err.message : String(err),
				);
				setLoadingHolders(false);
			});
	}, [api, isOpen, skill]);

	const targets = useMemo(
		() =>
			backendHolders
				? splitDeleteTargets(
						group.items,
						backendHolders,
						includeUnmanaged,
					)
				: { managed: [], unmanaged: [], named: [] },
		[group.items, backendHolders, includeUnmanaged],
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

			const scopes = [];
			if (globalItems.length > 0) {
				scopes.push({
					scope: "global" as const,
					projectRoot: null,
					agents: globalItems.map((item) => item.agent),
				});
			}
			if (projectItems.length > 0) {
				scopes.push({
					scope: "project" as const,
					projectRoot: projectPath ?? null,
					agents: projectItems.map((item) => item.agent),
				});
			}

			const unmanagedAgents = targets.unmanaged
				.map((item) => item.agent)
				.filter((agent): agent is string => !!agent);

			const res = await deleteSkill({
				api,
				queryClient,
				skillName: skill.name,
				t,
				scopes,
				intent: {
					kind: "all-agents",
					includeUnmanaged,
				},
				unmanagedAgents,
			});

			if (!res.success) {
				throw new Error(res.message || t("failedToDeleteSkill"));
			}
			return res;
		},
		onSuccess: (res) => {
			if (res?.unmanagedKept) {
				toast.info(t("deleteSkillKeptForUnmanaged"));
			}
			setIncludeUnmanaged(false);
			onClose();
		},
		onError: (error) => {
			console.error("Skill delete mutation error:", error);
			toast.danger(
				error instanceof Error
					? error.message
					: t("failedToDeleteSkill"),
			);
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
						{loadingHolders ? (
							<div className="flex items-center justify-center py-8">
								<Spinner size="md" />
							</div>
						) : holdersError ? (
							<div className="rounded-lg bg-danger/10 p-3 text-sm text-danger">
								{holdersError}
							</div>
						) : (
							<>
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
																? agentName(
																		item.agent,
																	)
																: t("default")}
														</span>
														{item.source_path && (
															<span className="flex-1 truncate text-xs text-muted">
																{
																	item.source_path
																}
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
																? agentName(
																		item.agent,
																	)
																: t("default")}
														</span>
														{item.source_path && (
															<span className="flex-1 truncate text-xs text-muted">
																{
																	item.source_path
																}
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
															? agentName(
																	item.agent,
																)
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
											isDisabled={
												deleteMutation.isPending
											}
										>
											<Checkbox.Content>
												<Checkbox.Control>
													<Checkbox.Indicator />
												</Checkbox.Control>
												<span className="text-sm">
													{t(
														"deleteSkillIncludeUnmanaged",
													)}
												</span>
											</Checkbox.Content>
										</Checkbox>
									</div>
								)}
							</>
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
								loadingHolders ||
								!!holdersError ||
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
