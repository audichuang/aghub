import { CheckIcon, PlusIcon } from "@heroicons/react/24/solid";
import { Spinner, toast } from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ScopeControl } from "../../components/scope-control";
import { useAgentAvailability } from "../../hooks/use-agent-availability";
import { useApi } from "../../hooks/use-api";
import { AgentIcon } from "../../lib/agent-icons";
import {
	supportsMcpScope,
	supportsSkillMutation,
} from "../../lib/agent-capabilities";
import {
	buildCoverageRows,
	type CoverageRow,
	groupResourcesByName,
	planCellToggle,
} from "../../lib/coverage-matrix";
import { cn } from "../../lib/utils";
import {
	mcpListQueryOptions,
	reconcileMcpsMutationOptions,
} from "../../requests/mcps";
import {
	reconcileSkillsMutationOptions,
	skillListQueryOptions,
} from "../../requests/skills";

type ResourceKind = "skill" | "mcp";

// Coverage overview grid: skills (+ mcp servers, global scope only) as rows,
// usable agents as columns. A cell toggles one resource on one agent via
// reconcile. This is the desktop face of the CLI `coverage` command.
//
// ponytail: renders every row directly (no virtualization). Skill/mcp counts
// here are dozens in practice; if a workspace ever holds hundreds, wrap the
// tbody in react-virtuoso (already a dep).
export default function CoveragePage() {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const { availableAgents } = useAgentAvailability();

	const [scope, setScope] = useState<"global" | "project">("global");
	const [projectPath, setProjectPath] = useState<string | null>(null);
	const [pendingCell, setPendingCell] = useState<string | null>(null);

	const projectRoot =
		scope === "project" ? (projectPath ?? undefined) : undefined;
	// MCP is global-only in this app, so the MCP section shows only in global
	// scope; project scope is skills-only.
	const showMcp = scope === "global";
	const skillsEnabled = scope === "global" || !!projectPath;

	const { data: skills = [], isLoading: skillsLoading } = useQuery({
		...skillListQueryOptions({ api, scope, projectRoot }),
		enabled: skillsEnabled,
	});
	const { data: mcps = [] } = useQuery({
		...mcpListQueryOptions({ api, scope: "global" }),
		enabled: showMcp,
	});

	const skillReconcile = useMutation(
		reconcileSkillsMutationOptions({ api, queryClient }),
	);
	const mcpReconcile = useMutation(
		reconcileMcpsMutationOptions({ api, queryClient }),
	);

	const skillAgents = useMemo(
		() =>
			(availableAgents ?? []).filter(
				(a) => a?.isUsable && supportsSkillMutation(a, scope),
			),
		[availableAgents, scope],
	);
	const mcpAgents = useMemo(
		() =>
			(availableAgents ?? []).filter(
				(a) => a?.isUsable && supportsMcpScope(a, "global"),
			),
		[availableAgents],
	);

	// One column axis shared by both sections: every agent that can carry either
	// kind, in availableAgents order. A cell greys out where its kind does not
	// apply to that agent.
	const columns = useMemo(() => {
		const seen = new Set<string>();
		const cols: { id: string; display_name: string }[] = [];
		for (const a of [...skillAgents, ...(showMcp ? mcpAgents : [])]) {
			if (seen.has(a.id)) continue;
			seen.add(a.id);
			cols.push({ id: a.id, display_name: a.display_name });
		}
		return cols;
	}, [skillAgents, mcpAgents, showMcp]);
	const columnIds = useMemo(() => columns.map((c) => c.id), [columns]);

	const skillAgentIds = useMemo(
		() => new Set(skillAgents.map((a) => a.id)),
		[skillAgents],
	);
	const mcpAgentIds = useMemo(
		() => new Set(mcpAgents.map((a) => a.id)),
		[mcpAgents],
	);

	const skillRows = useMemo(
		() =>
			buildCoverageRows(
				groupResourcesByName(skills, [...skillAgentIds]),
				columnIds,
				skillAgentIds,
			),
		[skills, skillAgentIds, columnIds],
	);
	const mcpRows = useMemo(
		() =>
			showMcp
				? buildCoverageRows(
						groupResourcesByName(mcps, [...mcpAgentIds]),
						columnIds,
						mcpAgentIds,
					)
				: [],
		[mcps, mcpAgentIds, columnIds, showMcp],
	);

	const handleToggle = async (
		kind: ResourceKind,
		row: CoverageRow,
		agentId: string,
	) => {
		const plan = planCellToggle(row.installedAgents, agentId);
		if (plan.kind === "blocked") {
			toast.danger(t("coverageLastInstallBlocked"));
			return;
		}
		if (plan.kind === "noop") return;

		const reconcile = kind === "skill" ? skillReconcile : mcpReconcile;
		const reconcileScope = kind === "mcp" ? "global" : scope;
		const reconcileProjectRoot =
			kind === "mcp"
				? null
				: scope === "project"
					? (projectPath ?? null)
					: null;

		setPendingCell(`${kind}:${row.name}:${agentId}`);
		try {
			const result = await reconcile.mutateAsync({
				source: {
					agent: plan.sourceAgent,
					scope: reconcileScope,
					project_root: reconcileProjectRoot,
					name: row.name,
				},
				added: plan.added.length > 0 ? plan.added : null,
				removed: plan.removed.length > 0 ? plan.removed : null,
			});
			if (result.failed_count > 0) {
				toast.danger(t("coverageToggleFailed"));
			}
		} catch {
			toast.danger(t("coverageToggleFailed"));
		} finally {
			setPendingCell(null);
		}
	};

	const renderRows = (kind: ResourceKind, rows: CoverageRow[]) =>
		rows.map((row) => (
			<tr key={`${kind}:${row.name}`} className="group">
				<td className="sticky left-0 z-10 bg-background py-1.5 pr-4 pl-1 group-hover:bg-surface-secondary">
					<span className="text-sm text-foreground">{row.name}</span>
				</td>
				{row.cells.map((cell) => {
					const cellKey = `${kind}:${row.name}:${cell.agentId}`;
					const isPending = pendingCell === cellKey;
					return (
						<td
							key={cell.agentId}
							className="px-2 py-1.5 text-center group-hover:bg-surface-secondary"
						>
							{cell.applicable ? (
								<button
									type="button"
									disabled={isPending || pendingCell !== null}
									aria-pressed={cell.installed}
									aria-label={row.name}
									onClick={() =>
										void handleToggle(
											kind,
											row,
											cell.agentId,
										)
									}
									className={cn(
										"inline-flex size-6 items-center justify-center rounded-md border transition-colors",
										cell.installed
											? "border-accent bg-accent/15 text-accent"
											: "border-separator text-transparent hover:border-accent/50 hover:text-accent/60",
										pendingCell !== null &&
											"cursor-not-allowed opacity-60",
									)}
								>
									{isPending ? (
										<Spinner size="sm" />
									) : cell.installed ? (
										<CheckIcon className="size-4" />
									) : (
										<PlusIcon className="size-3.5" />
									)}
								</button>
							) : (
								<span className="text-xs text-muted/40">–</span>
							)}
						</td>
					);
				})}
			</tr>
		));

	const hasColumns = columns.length > 0;
	const hasRows = skillRows.length > 0 || mcpRows.length > 0;

	return (
		<div className="flex h-full flex-col overflow-hidden">
			<div className="flex items-center justify-between gap-3 border-b border-separator px-4 py-3">
				<div className="grid gap-0.5">
					<h1 className="text-base font-semibold text-foreground">
						{t("coverage")}
					</h1>
					<p className="text-xs text-muted">
						{t("coverageDescription")}
					</p>
				</div>
				<ScopeControl
					scope={scope}
					selectedProjectPath={projectPath}
					onChange={(s, p) => {
						setScope(s);
						setProjectPath(p);
					}}
				/>
			</div>

			<div className="min-h-0 flex-1 overflow-auto p-4">
				{scope === "project" && !projectPath ? (
					<p className="text-sm text-muted">
						{t("coverageSelectProject")}
					</p>
				) : skillsLoading ? (
					<div className="flex justify-center py-10">
						<Spinner />
					</div>
				) : !hasColumns ? (
					<p className="text-sm text-muted">{t("noTargetAgents")}</p>
				) : !hasRows ? (
					<p className="text-sm text-muted">{t("coverageEmpty")}</p>
				) : (
					<table className="border-collapse text-left">
						<thead>
							<tr>
								<th className="sticky left-0 z-20 bg-background pb-2 pl-1" />
								{columns.map((c) => (
									<th
										key={c.id}
										className="px-2 pb-2 align-bottom font-normal"
									>
										<div className="flex flex-col items-center gap-1">
											<AgentIcon
												id={c.id}
												name={c.display_name}
												size="xs"
											/>
											<span className="max-w-[72px] truncate text-xs text-muted">
												{c.display_name}
											</span>
										</div>
									</th>
								))}
							</tr>
						</thead>
						<tbody>
							{skillRows.length > 0 && (
								<>
									<tr>
										<td
											colSpan={columns.length + 1}
											className="sticky left-0 pt-3 pb-1 pl-1 text-xs font-semibold tracking-wide text-muted uppercase"
										>
											{t("skills")}
										</td>
									</tr>
									{renderRows("skill", skillRows)}
								</>
							)}
							{showMcp && mcpRows.length > 0 && (
								<>
									<tr>
										<td
											colSpan={columns.length + 1}
											className="sticky left-0 pt-4 pb-1 pl-1 text-xs font-semibold tracking-wide text-muted uppercase"
										>
											{t("mcpServers")}
										</td>
									</tr>
									{renderRows("mcp", mcpRows)}
								</>
							)}
						</tbody>
					</table>
				)}
			</div>
		</div>
	);
}
