import { useMemo } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "@heroui/react";
import { useTranslation } from "react-i18next";
import type { ConfigSource } from "../generated/dto";
import { useApi } from "../hooks/use-api";
import { bulkFailureItemsLabel } from "../lib/bulk-errors";
import type { DeleteFn } from "../lib/delete-preview";
import { invalidateMcpQueries } from "../requests/mcps";
import { invalidateSkillQueries } from "../requests/skills";
import { DeletePreviewDialog } from "./delete-preview-dialog";

interface BulkDeleteItem {
	name: string;
	agent?: string | null;
	source?: ConfigSource | null;
	source_path?: string | null;
}

interface BulkDeleteGroup {
	key: string;
	items: BulkDeleteItem[];
	resourceType?: "mcp" | "skill";
}

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
	const queryClient = useQueryClient();

	// Build one delete closure per de-duped item; keep its name/agent in lockstep
	// (same index) so a confirm-phase failure can be named precisely.
	const { deleteFns, info } = useMemo(() => {
		const fns: DeleteFn[] = [];
		const info: Array<{ name: string; agent: string }> = [];
		const seen = new Set<string>();
		for (const group of groups) {
			const groupResourceType = group.resourceType ?? resourceType;
			for (const item of group.items) {
				if (!item.agent) continue;
				const agent = item.agent;
				const scope: "global" | "project" = item.source ?? "global";
				const projectRoot =
					scope === "project" ? projectPath : undefined;
				const dedupKey =
					groupResourceType === "skill" && item.source_path
						? `skill:${item.source_path}:${scope}`
						: groupResourceType === "skill"
							? `skill:${item.agent}:${group.key}:${scope}`
							: `${groupResourceType}:${item.agent}:${item.name}:${scope}`;
				if (seen.has(dedupKey)) continue;
				seen.add(dedupKey);
				if (groupResourceType === "mcp") {
					fns.push((confirm) =>
						api.mcps.delete(
							item.name,
							agent,
							scope,
							projectRoot,
							confirm,
						),
					);
				} else {
					fns.push((confirm) =>
						api.skills.delete(
							agent,
							group.key,
							scope,
							projectRoot,
							false,
							confirm,
						),
					);
				}
				info.push({ name: item.name, agent });
			}
		}
		return { deleteFns: fns, info };
	}, [groups, resourceType, projectPath, api]);

	const confirmKey =
		resourceType === "mcp"
			? "bulkDeleteMcpConfirm"
			: resourceType === "skill"
				? "bulkDeleteSkillConfirm"
				: "bulkDeleteMixedConfirm";

	const invalidate = async () => {
		if (resourceType === "mcp" || resourceType === "mixed") {
			await invalidateMcpQueries(queryClient);
		}
		if (resourceType === "skill" || resourceType === "mixed") {
			await invalidateSkillQueries(queryClient);
		}
	};

	return (
		<DeletePreviewDialog
			isOpen={isOpen}
			onClose={onClose}
			deleteFns={deleteFns}
			heading={t("bulkDeleteConfirmTitle")}
			description={t(confirmKey, { count: groups.length })}
			confirmLabel={t("deleteSelected")}
			onConfirmed={async () => {
				await invalidate();
				onSuccess();
			}}
			onFailed={async (failed) => {
				await invalidate();
				const failures = failed.map((i) => info[i]);
				console.error(
					`${resourceType} bulk delete failures:`,
					failures,
				);
				toast.danger(
					t("bulkDeleteFailedItems", bulkFailureItemsLabel(failures)),
				);
			}}
		/>
	);
}
