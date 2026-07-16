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
		rows.map((row, index) => {
			const isEven = index % 2 === 0;
			const rowBgClass = isEven ? "bg-surface-secondary" : "bg-surface";
			const stickyBgClass = isEven
				? "bg-surface-secondary"
				: "bg-surface";

			return (
				<tr
					key={`${kind}:${row.name}`}
					className={cn(
						"group border-b border-separator/30 last:border-0 transition-colors duration-150",
						rowBgClass,
						"hover:bg-surface-tertiary",
					)}
				>
					<td
						className={cn(
							"sticky left-0 z-10 py-2 px-4 font-medium transition-colors duration-150 border-r border-b border-separator/30",
							stickyBgClass,
							"group-hover:bg-surface-tertiary",
						)}
					>
						<span className="text-sm font-medium text-foreground">
							{row.name}
						</span>
					</td>
					{row.cells.map((cell) => {
						const cellKey = `${kind}:${row.name}:${cell.agentId}`;
						const isPending = pendingCell === cellKey;
						return (
							<td
								key={cell.agentId}
								className="px-3 py-2 text-center transition-colors duration-150 border-b border-separator/20"
							>
								<div className="flex items-center justify-center">
									{cell.applicable ? (
										<button
											type="button"
											disabled={
												isPending ||
												pendingCell !== null
											}
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
												"inline-flex size-6 items-center justify-center rounded-md border transition-all duration-200 shadow-xs",
												cell.installed
													? "border-accent bg-accent/15 text-accent hover:bg-accent/25 hover:border-accent"
													: "border-separator bg-surface-secondary/40 text-muted/50 hover:border-accent/50 hover:bg-accent/10 hover:text-accent",
												pendingCell !== null &&
													"cursor-not-allowed opacity-60",
											)}
										>
											{isPending ? (
												<Spinner size="sm" />
											) : cell.installed ? (
												<CheckIcon className="size-3.5" />
											) : (
												<PlusIcon className="size-3.5 opacity-60 group-hover:opacity-100 transition-opacity" />
											)}
										</button>
									) : (
										<span className="text-xs text-muted/30 font-semibold select-none">
											–
										</span>
									)}
								</div>
							</td>
						);
					})}
				</tr>
			);
		});

	const hasColumns = columns.length > 0;
	const hasRows = skillRows.length > 0 || mcpRows.length > 0;

	return (
		<div className="flex h-full flex-col overflow-hidden">
			<div className="flex items-center justify-between gap-3 border-b border-separator px-6 py-4 bg-surface/50 backdrop-blur-md">
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

			<div className="min-h-0 flex-1 overflow-auto p-6 flex flex-col">
				{scope === "project" && !projectPath ? (
					<div className="flex-1 flex flex-col items-center justify-center p-8 text-center">
						<p className="text-sm text-muted">
							{t("coverageSelectProject")}
						</p>
					</div>
				) : skillsLoading ? (
					<div className="flex-1 flex items-center justify-center py-10">
						<Spinner size="lg" />
					</div>
				) : !hasColumns ? (
					<div className="flex-1 flex flex-col items-center justify-center p-8 text-center">
						<p className="text-sm text-muted">
							{t("noTargetAgents")}
						</p>
					</div>
				) : !hasRows ? (
					<div className="flex-1 flex flex-col items-center justify-center p-8 text-center">
						<p className="text-sm text-muted">
							{t("coverageEmpty")}
						</p>
					</div>
				) : (
					<div className="border border-separator rounded-xl bg-surface shadow-xs overflow-auto max-h-full">
						<table className="w-full border-separate border-spacing-0 text-left">
							<thead>
								<tr className="bg-surface-secondary">
									<th className="sticky left-0 top-0 z-30 bg-surface-secondary px-4 py-3 min-w-[220px] border-r border-b border-separator/40" />
									{columns.map((c) => (
										<th
											key={c.id}
											className="sticky top-0 z-20 bg-surface-secondary px-3 py-3.5 align-bottom font-normal w-24 min-w-24 max-w-24 text-center border-b border-separator/40"
										>
											<div className="flex flex-col items-center gap-1.5">
												<AgentIcon
													id={c.id}
													name={c.display_name}
													size="xs"
												/>
												<span className="max-w-[80px] truncate text-xs font-semibold text-muted hover:text-foreground transition-colors select-none">
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
												className="sticky left-0 z-10 bg-surface-secondary/70 px-4 py-2.5 border-b border-separator/30 text-xs font-semibold tracking-wider text-muted uppercase"
											>
												<div className="flex items-center gap-2">
													<span className="h-3.5 w-1 rounded-full bg-accent" />
													<span className="font-bold text-foreground/80">
														{t("skills")}
													</span>
													<span className="rounded-full bg-separator/50 px-1.5 py-0.5 text-[10px] font-semibold text-muted normal-case select-none">
														{skillRows.length}
													</span>
												</div>
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
												className="sticky left-0 z-10 bg-surface-secondary/70 px-4 py-2.5 border-b border-separator/30 text-xs font-semibold tracking-wider text-muted uppercase"
											>
												<div className="flex items-center gap-2">
													<span className="h-3.5 w-1 rounded-full bg-success" />
													<span className="font-bold text-foreground/80">
														{t("mcpServers")}
													</span>
													<span className="rounded-full bg-separator/50 px-1.5 py-0.5 text-[10px] font-semibold text-muted normal-case select-none">
														{mcpRows.length}
													</span>
												</div>
											</td>
										</tr>
										{renderRows("mcp", mcpRows)}
									</>
								)}
							</tbody>
						</table>
					</div>
				)}
			</div>
		</div>
	);
}
