import { ExclamationTriangleIcon } from "@heroicons/react/24/solid";
import { Button, Checkbox, Modal, Spinner, toast } from "@heroui/react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { SkillResponse } from "../generated/dto";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { useApi } from "../hooks/use-api";
import { supportsSkillMutation } from "../lib/agent-capabilities";
import { deleteSkill } from "../requests/delete-skill";
import { queryKeys } from "../requests/keys";
import {
	invalidateSkillQueries,
	withheldSkillsQueryOptions,
} from "../requests/skills";

interface WithheldSkillsRowProps {
	scope: "global" | "project";
	projectPath?: string;
}

/**
 * Skills stored in `.aghub` that NO agent reads — every agent was unticked.
 *
 * Every per-agent list misses them by construction, so without this row such
 * a skill is invisible: it cannot be re-granted or deleted from the app, yet
 * it still turns up in update checks. The row names the state; the dialog
 * offers the only two ways out (grant it again, or delete it).
 *
 * Renders nothing when there is nothing to report, so `SkillStatusStrip`'s
 * `empty:hidden` still hides the whole strip.
 */
export function WithheldSkillsRow({
	scope,
	projectPath,
}: WithheldSkillsRowProps) {
	const { t } = useTranslation();
	const api = useApi();
	const [isOpen, setIsOpen] = useState(false);
	const { data: skills = [] } = useQuery(
		withheldSkillsQueryOptions({ api, scope, projectRoot: projectPath }),
	);

	if (skills.length === 0) return null;

	return (
		<>
			<div className="flex items-center gap-2 px-3 py-2">
				<ExclamationTriangleIcon className="size-4 shrink-0 text-warning" />
				<span className="min-w-0 flex-1 truncate text-foreground">
					{t("withheldSkillsRow", { count: skills.length })}
				</span>
				<Button
					size="sm"
					variant="ghost"
					className="shrink-0"
					onPress={() => setIsOpen(true)}
				>
					{t("withheldSkillsReview")}
				</Button>
			</div>

			<Modal.Backdrop
				isOpen={isOpen}
				onOpenChange={() => setIsOpen(false)}
			>
				<Modal.Container>
					<Modal.Dialog className="flex max-h-[85vh] w-[calc(100vw-2rem)] max-w-md flex-col overflow-hidden sm:max-w-lg">
						<Modal.CloseTrigger />
						<Modal.Header>
							<div className="flex items-center gap-2">
								<ExclamationTriangleIcon className="size-5 text-warning" />
								<Modal.Heading>
									{t("withheldSkillsTitle")}
								</Modal.Heading>
							</div>
						</Modal.Header>
						<Modal.Body className="flex min-h-0 flex-1 flex-col overflow-y-auto p-4">
							<p className="mb-3 text-sm text-muted">
								{t("withheldSkillsExplain")}
							</p>
							<ul className="space-y-3">
								{skills.map((skill) => (
									<WithheldSkillItem
										key={skill.name}
										skill={skill}
										scope={scope}
										projectPath={projectPath}
									/>
								))}
							</ul>
						</Modal.Body>
						<Modal.Footer>
							<Button
								slot="close"
								variant="secondary"
								size="md"
								onPress={() => setIsOpen(false)}
								className="min-h-[44px]"
							>
								{t("withheldSkillsClose")}
							</Button>
						</Modal.Footer>
					</Modal.Dialog>
				</Modal.Container>
			</Modal.Backdrop>
		</>
	);
}

function WithheldSkillItem({
	skill,
	scope,
	projectPath,
}: {
	skill: SkillResponse;
	scope: "global" | "project";
	projectPath?: string;
}) {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const { availableAgents } = useAgentAvailability();
	const agents = useMemo(
		() =>
			availableAgents.filter(
				(a) => a.isUsable && supportsSkillMutation(a, scope),
			),
		[availableAgents, scope],
	);
	const [picked, setPicked] = useState<Set<string>>(() => new Set());
	const [confirmDelete, setConfirmDelete] = useState(false);
	const [busy, setBusy] = useState<"grant" | "delete" | null>(null);

	const refresh = async () => {
		await invalidateSkillQueries(queryClient);
		await queryClient.refetchQueries({
			queryKey: queryKeys.skills.withheld(scope, projectPath),
		});
	};

	const grant = async () => {
		const path = skill.source_path;
		if (!path) return;
		setBusy("grant");
		const failed: string[] = [];
		// One agent per request: the import route is per-agent, and it reuses
		// the existing Master without touching its lock entry (so the skill
		// keeps its upstream source and stays updatable).
		for (const agentId of picked) {
			try {
				await api.skills.import(agentId, { path }, projectPath);
			} catch (error) {
				failed.push(
					`${agentId}: ${error instanceof Error ? error.message : String(error)}`,
				);
			}
		}
		setBusy(null);
		if (failed.length > 0) {
			toast.danger(t("withheldSkillsGrantFailed", { name: skill.name }), {
				description: (
					<span className="whitespace-pre-line">
						{failed.join("\n")}
					</span>
				),
				timeout: 0,
			});
		} else {
			toast.success(
				t("withheldSkillsGranted", {
					name: skill.name,
					count: picked.size,
				}),
			);
		}
		await refresh();
	};

	const remove = async () => {
		const agentId = agents[0]?.id;
		if (!agentId) return;
		setBusy("delete");
		try {
			const res = await deleteSkill({
				api,
				queryClient,
				skillName: skill.name,
				t,
				scope,
				projectRoot: projectPath,
				agent: agentId,
				intent: {
					kind: "all-agents",
				},
			});
			if (!res.success) {
				throw new Error(res.message || t("failedToDeleteSkill"));
			}
			toast.success(t("withheldSkillsDeleted", { name: skill.name }));
		} catch (error) {
			toast.danger(
				t("withheldSkillsDeleteFailed", { name: skill.name }),
				{
					description:
						error instanceof Error ? error.message : String(error),
				},
			);
		} finally {
			setBusy(null);
			setConfirmDelete(false);
		}
		await queryClient.refetchQueries({
			queryKey: queryKeys.skills.withheld(scope, projectPath),
		});
	};

	return (
		<li className="rounded-md border border-separator p-3 text-sm">
			<div className="font-medium">{skill.name}</div>
			{skill.description && (
				<p className="mt-0.5 line-clamp-2 text-muted text-xs">
					{skill.description}
				</p>
			)}
			<p className="mt-2 mb-1 text-muted text-xs">
				{t("withheldSkillsPickAgents")}
			</p>
			<div className="flex flex-wrap gap-x-3 gap-y-1">
				{agents.map((agent) => (
					<Checkbox
						key={agent.id}
						isSelected={picked.has(agent.id)}
						isDisabled={busy !== null}
						onChange={(selected) =>
							setPicked((prev) => {
								const next = new Set(prev);
								if (selected) next.add(agent.id);
								else next.delete(agent.id);
								return next;
							})
						}
					>
						<Checkbox.Content>
							<Checkbox.Control>
								<Checkbox.Indicator />
							</Checkbox.Control>
							<span className="text-xs">
								{agent.display_name}
							</span>
						</Checkbox.Content>
					</Checkbox>
				))}
			</div>
			<div className="mt-3 flex justify-end gap-2">
				{confirmDelete ? (
					<>
						<Button
							size="sm"
							variant="secondary"
							isDisabled={busy !== null}
							onPress={() => setConfirmDelete(false)}
						>
							{t("cancel")}
						</Button>
						<Button
							size="sm"
							variant="danger"
							isDisabled={busy !== null || agents.length === 0}
							aria-label={t("withheldSkillsConfirmDelete")}
							onPress={() => void remove()}
						>
							{busy === "delete" ? (
								<Spinner size="sm" color="current" />
							) : (
								t("withheldSkillsConfirmDelete")
							)}
						</Button>
					</>
				) : (
					<>
						<Button
							size="sm"
							variant="ghost"
							isDisabled={busy !== null}
							onPress={() => setConfirmDelete(true)}
						>
							{t("delete")}
						</Button>
						<Button
							size="sm"
							variant="primary"
							isDisabled={
								busy !== null ||
								picked.size === 0 ||
								!skill.source_path
							}
							aria-label={t("withheldSkillsGrant")}
							onPress={() => void grant()}
						>
							{busy === "grant" ? (
								<Spinner size="sm" color="current" />
							) : (
								t("withheldSkillsGrant")
							)}
						</Button>
					</>
				)}
			</div>
		</li>
	);
}
