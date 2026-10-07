import { ExclamationTriangleIcon } from "@heroicons/react/24/solid";
import { Button, Checkbox, Modal, Spinner, toast } from "@heroui/react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { useAgentName } from "../hooks/use-agent-name";
import { useApi } from "../hooks/use-api";
import { BulkOperationError, bulkFailureItemsLabel } from "../lib/bulk-errors";
import {
	buildBulkDeleteRequests,
	collectUnmanagedDeleteTargets,
	type BulkDeleteGroup,
} from "../lib/skill-delete-targets";
import { invalidateMcpQueries } from "../requests/mcps";
import { invalidateSkillQueries } from "../requests/skills";

interface BulkDeleteDialogProps {
	groups: BulkDeleteGroup[];
	isOpen: boolean;
	onClose: () => void;
	onSuccess: () => void;
	resourceType: "mcp" | "skill" | "mixed";
	projectPath?: string;
}

export function BulkDeleteDialog({
	groups,
	isOpen,
	onClose,
	onSuccess,
	resourceType,
	projectPath,
}: BulkDeleteDialogProps) {
	const { t } = useTranslation();
	const api = useApi();
	const agentName = useAgentName();
	const queryClient = useQueryClient();
	const { availableAgents } = useAgentAvailability();

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

	const unmanagedItems = useMemo(
		() =>
			collectUnmanagedDeleteTargets(
				groups,
				managedAgentIds,
				resourceType,
			),
		[groups, managedAgentIds, resourceType],
	);

	const unmanagedAgents = useMemo(
		() => [
			...new Set(
				unmanagedItems
					.map((item) => item.agent)
					.filter((agent): agent is string => !!agent),
			),
		],
		[unmanagedItems],
	);
	const hasUnmanaged = unmanagedAgents.length > 0;

	const handleClose = () => {
		setIncludeUnmanaged(false);
		onClose();
	};

	const { requests, skippedGroupKeys } = useMemo(
		() =>
			buildBulkDeleteRequests({
				groups,
				resourceType,
				managedAgentIds,
				includeUnmanaged,
				projectPath,
			}),
		[groups, resourceType, managedAgentIds, includeUnmanaged, projectPath],
	);

	const deleteMutation = useMutation({
		mutationFn: async () => {
			const promises: Promise<void>[] = [];
			const deleteInfo: Array<{
				name: string;
				agent: string;
				scope: string;
			}> = [];

			for (const req of requests) {
				if (req.resourceType === "mcp") {
					promises.push(
						api.mcps.delete(
							req.name,
							req.agent,
							req.scope,
							req.projectRoot,
							req.agents,
						),
					);
				} else {
					promises.push(
						api.skills
							.delete(
								req.agent,
								req.groupKey,
								req.scope,
								req.projectRoot,
								false,
								req.agents,
							)
							.then((result) => {
								// HTTP 200 is not "deleted": `kept` removed
								// nothing and `partial` left paths behind.
								// See docs/history/desktop-frontend.md#bulk-delete-counted-kept-as-deleted
								if (
									result.outcome === "kept" ||
									result.outcome === "partial"
								) {
									throw new Error(
										t(
											result.outcome === "kept"
												? "bulkDeleteKept"
												: "bulkDeletePartial",
										),
									);
								}
							}),
					);
				}
				deleteInfo.push({
					name: req.name,
					agent: req.agent,
					scope: req.scope,
				});
			}

			const results = await Promise.allSettled(promises);
			const failures: Array<{
				name: string;
				agent?: string | null;
				error: string | null;
			}> = results
				.map((r, i) => ({ result: r, info: deleteInfo[i] }))
				.filter(({ result }) => result.status === "rejected")
				.map(({ result, info }) => ({
					name: info.name,
					agent: info.agent,
					error:
						(result as PromiseRejectedResult).reason instanceof
						Error
							? (
									(result as PromiseRejectedResult)
										.reason as Error
								).message
							: null,
				}));

			for (const key of skippedGroupKeys) {
				const group = groups.find((g) => g.key === key);
				failures.push({
					name: group?.items[0]?.name ?? key,
					agent: group?.items[0]?.agent ?? null,
					error: t("bulkDeleteKept"),
				});
			}

			if (failures.length > 0) {
				console.error(
					`${resourceType} bulk delete failures:`,
					failures,
				);
				throw new BulkOperationError(failures);
			}
			return { deleted: promises.length };
		},
		onSuccess: async () => {
			if (resourceType === "mcp" || resourceType === "mixed") {
				await invalidateMcpQueries(queryClient);
			}
			if (resourceType === "skill" || resourceType === "mixed") {
				await invalidateSkillQueries(queryClient);
			}
			setIncludeUnmanaged(false);
			onSuccess();
			onClose();
		},
		onError: (error) => {
			console.error("Bulk delete mutation error:", error);
			if (error instanceof BulkOperationError) {
				toast.danger(
					t(
						"bulkDeleteFailedItems",
						bulkFailureItemsLabel(error.failures),
					),
				);
				return;
			}
			toast.danger(
				error instanceof Error ? error.message : t("bulkDeleteFailed"),
			);
		},
	});

	const confirmKey =
		resourceType === "mcp"
			? "bulkDeleteMcpConfirm"
			: resourceType === "skill"
				? "bulkDeleteSkillConfirm"
				: "bulkDeleteMixedConfirm";

	return (
		<Modal.Backdrop isOpen={isOpen} onOpenChange={handleClose}>
			<Modal.Container>
				<Modal.Dialog>
					<Modal.CloseTrigger />
					<Modal.Header>
						<div className="flex items-center gap-2">
							<ExclamationTriangleIcon className="size-5 text-warning" />
							<Modal.Heading>
								{t("bulkDeleteConfirmTitle")}
							</Modal.Heading>
						</div>
					</Modal.Header>
					<Modal.Body>
						<p className="text-sm text-muted">
							{t(confirmKey, {
								count: groups.length,
							})}
						</p>
						{hasUnmanaged && (
							<div className="mt-4 rounded-lg bg-surface-secondary p-3">
								<h4
									className="
										mb-1 text-xs font-medium tracking-wide text-muted
										uppercase
									"
								>
									{t("bulkDeleteUnmanagedTitle")}
								</h4>
								<p className="mb-2 text-xs text-muted">
									{t("bulkDeleteUnmanagedHint")}
								</p>
								<div className="mb-3 space-y-1">
									{unmanagedAgents.map((agent) => (
										<div
											key={agent}
											className="flex items-center gap-2 text-sm"
										>
											<span className="text-foreground">
												{agentName(agent)}
											</span>
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
							size="md"
							onPress={handleClose}
							isDisabled={deleteMutation.isPending}
							className="min-h-[44px]"
						>
							{t("cancel")}
						</Button>
						<Button
							variant="danger"
							size="md"
							onPress={() => deleteMutation.mutate()}
							isDisabled={
								deleteMutation.isPending ||
								requests.length === 0
							}
							className="min-h-[44px] min-w-[120px]"
						>
							{deleteMutation.isPending ? (
								<Spinner size="sm" color="current" />
							) : (
								t("deleteSelected")
							)}
						</Button>
					</Modal.Footer>
				</Modal.Dialog>
			</Modal.Container>
		</Modal.Backdrop>
	);
}
