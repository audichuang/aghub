import { ArrowPathIcon } from "@heroicons/react/24/outline";
import { Alert, Button, Modal, Spinner, toast } from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { RepairReportDto } from "../generated/dto";
import { useApi } from "../hooks/use-api";
import {
	migrationBannerModel,
	migrationRowFacts,
	migrationSummary,
} from "../lib/skill-migration";
import { queryKeys } from "../requests/keys";
import {
	repairPreviewQueryOptions,
	repairSkillsMutationOptions,
} from "../requests/skills";

interface SkillLayoutMigrationBannerProps {
	scope: "global" | "project";
	projectPath?: string;
}

/**
 * "Some skills at this scope still use the old layout" — with a preview.
 *
 * The banner is NOT a nag: it only appears when a dry run actually found
 * something to do, and it says what migrating buys (per-agent links, so a skill
 * can be revoked for one agent) and what it does not (the agents with no
 * private directory stay fused to the shared slot). A user who cannot see that
 * distinction does not know what the button did.
 *
 * Persistent state, not a toast: this stays true until the user acts on it, and
 * desktop `AGENTS.md` reserves toasts for transient events. So it carries
 * `role="alert"` + `aria-live="polite"` — an inline banner with no live-region
 * semantics is silent to a screen reader.
 */
export function SkillLayoutMigrationBanner({
	scope,
	projectPath,
}: SkillLayoutMigrationBannerProps) {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const [isOpen, setIsOpen] = useState(false);

	const { data, isSuccess } = useQuery(
		repairPreviewQueryOptions({ api, scope, projectRoot: projectPath }),
	);

	const repair = useMutation({
		...repairSkillsMutationOptions({
			api,
			queryClient,
			onSuccess: async (result) => {
				// The preview is what the banner reads, and a real run just
				// changed the answer. Without this the banner keeps offering a
				// migration that already happened.
				await queryClient.invalidateQueries({
					queryKey: queryKeys.skills.repairPreviews(),
				});
				if (result.refused) {
					// Deliberately NOT auto-closing: the refusal rows carry the
					// literal next command, and closing the dialog would throw
					// away the only place the user can read it.
					return;
				}
				setIsOpen(false);
				toast.success(
					t("skillLayoutMigrated", { count: result.skills.length }),
				);
			},
			// A bulk repair aborts on the first failing skill, so this fires
			// with an unknown number of skills ALREADY migrated. The banner
			// re-reads its preview either way (see the mutation options), but
			// silence would leave the user watching the list shrink with no
			// idea why — and the whole complaint about this flow is not
			// knowing what it did.
			onError: (error) =>
				toast.danger(
					t("skillLayoutMigrateFailed", {
						message:
							error instanceof Error
								? error.message
								: String(error),
					}),
				),
		}),
	});

	const { visible, rows } = migrationBannerModel(data, isSuccess);
	if (!visible) {
		return null;
	}

	const result = repair.data;
	const shown: RepairReportDto[] = result ? result.skills : rows;
	const done = result ? !result.dry_run : false;
	// The store path and the fused-agent sentence are the same for every row.
	// Repeated per skill they buried the list; hoisted they are one sentence.
	const summary = migrationSummary(shown);

	return (
		<>
			<Alert status="warning" role="alert" aria-live="polite">
				<Alert.Indicator />
				<Alert.Content>
					<Alert.Title>{t("skillLayoutOutdatedTitle")}</Alert.Title>
					<Alert.Description>
						{t("skillLayoutOutdatedHint", { count: rows.length })}
					</Alert.Description>
					{/* Inside Alert.Content, wrapped — the shape every other
					    Alert-with-action in this app uses (see
					    `source-detail.tsx`'s orphan-lock and prune-retry
					    alerts). A Button as a direct sibling of Alert.Content
					    is not a layout HeroUI v3 promises anything about. */}
					<div className="mt-3">
						<Button
							variant="secondary"
							size="sm"
							onPress={() => setIsOpen(true)}
						>
							{t("skillLayoutReview")}
						</Button>
					</div>
				</Alert.Content>
			</Alert>

			<Modal.Backdrop
				isOpen={isOpen}
				onOpenChange={() => setIsOpen(false)}
			>
				<Modal.Container>
					{/* The height bound is what makes `Modal.Body`'s
					    `min-h-0 flex-1 overflow-y-auto` actually scroll — without
					    it the dialog just grows past the viewport and the footer
					    (with the Migrate button) is unreachable. Same shape as
					    `bulk-manage-group-agents-dialog`. */}
					<Modal.Dialog className="flex max-h-[85vh] w-[calc(100vw-2rem)] max-w-md flex-col overflow-hidden sm:max-w-lg">
						<Modal.CloseTrigger />
						<Modal.Header>
							<div className="flex items-center gap-2">
								<ArrowPathIcon className="size-5 text-warning" />
								<Modal.Heading>
									{t("skillLayoutMigrateTitle")}
								</Modal.Heading>
							</div>
						</Modal.Header>
						{/* Scrolls, matching `bulk-manage-group-agents-dialog`.
						    Without it a bulk migration of twenty skills pushed
						    the footer off-screen and the Migrate button became
						    unreachable — visible only by rendering it with a
						    realistic list. */}
						<Modal.Body className="flex min-h-0 flex-1 flex-col overflow-y-auto p-4">
							<p className="mb-3 text-sm text-muted">
								{t("skillLayoutMigrateExplain")}
							</p>
							<div className="mb-3 space-y-1 rounded-md border border-default bg-surface-secondary p-3 text-muted text-xs">
								{summary.masterParent !== null && (
									<p className="break-all">
										{t(
											done
												? "skillLayoutSummaryDone"
												: "skillLayoutSummary",
											{
												count: summary.migrating,
												path: summary.masterParent,
												links: summary.totalLinks,
											},
										)}
									</p>
								)}
								{summary.fused.length > 0 && (
									// The honest half, said ONCE. These agents
									// do not become individually revocable, and
									// that is a property of the scope, not of
									// any one skill.
									<p>
										{t("skillLayoutStillShared", {
											agents: summary.fused.join(", "),
										})}
									</p>
								)}
							</div>
							<ul className="space-y-1">
								{shown.map((row) => (
									<MigrationRow
										key={row.name}
										row={row}
										// After a real run these rows describe
										// what HAPPENED. Reusing the preview's
										// future-tense label made a finished
										// migration read as a plan that had not
										// run yet.
										done={done}
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
								isDisabled={repair.isPending}
								className="min-h-[44px]"
							>
								{t("cancel")}
							</Button>
							<Button
								variant="primary"
								size="md"
								onPress={() =>
									repair.mutate({
										scope,
										projectRoot: projectPath,
										dryRun: false,
									})
								}
								isDisabled={repair.isPending}
								className="min-h-[44px] min-w-[140px]"
							>
								{repair.isPending ? (
									<Spinner size="sm" color="current" />
								) : (
									// Re-running after a partial failure is
									// safe — repair is idempotent — so the
									// label offers it rather than making the
									// user reset anything.
									t(
										result
											? "skillLayoutRunAgain"
											: "skillLayoutApply",
									)
								)}
							</Button>
						</Modal.Footer>
					</Modal.Dialog>
				</Modal.Container>
			</Modal.Backdrop>
		</>
	);
}

/**
 * One skill's row — ONE LINE for anything that is going fine.
 *
 * The store path and the fused set moved into the dialog's summary, so all a
 * healthy row still has to answer is which skill and how many links. A refusal
 * stays expanded: it carries the literal command that unsticks the user, and
 * that is per-skill by nature.
 */
function MigrationRow({ row, done }: { row: RepairReportDto; done: boolean }) {
	const { t } = useTranslation();
	// Derived in `lib/skill-migration`, which node can test — this component
	// only lays the facts out.
	const { refused, linkCount } = migrationRowFacts(row);

	if (refused) {
		return (
			<li className="rounded-md border border-danger/30 p-3 text-sm">
				<div className="flex items-center justify-between gap-2">
					<span className="font-medium">{row.name}</span>
					<span className="text-danger text-xs">
						{t(`skillRepairOutcome_${done ? "done_" : ""}refused`)}
					</span>
				</div>
				<p className="mt-2 text-danger text-xs">{row.reason}</p>
				{/* Shown verbatim and selectable: it is the literal command
				    that unsticks the user. Paraphrasing it would leave them
				    with a diagnosis and no way out.
				    NOT `bg-muted` — that token is a FOREGROUND grey
				    (`--muted`), so it painted the box in the text colour and
				    the command rendered as a blank bar. `bg-surface-secondary`
				    + `text-foreground` is what every other code block in this
				    app uses. Only visible by actually rendering it. */}
				<pre className="mt-1 max-h-28 select-text overflow-auto whitespace-pre-wrap break-all rounded-lg bg-surface-secondary px-3 py-2 font-mono text-foreground text-xs">
					{row.fix}
				</pre>
			</li>
		);
	}

	return (
		<li className="flex items-center justify-between gap-3 px-1 py-1 text-sm">
			<span className="truncate">{row.name}</span>
			<span className="shrink-0 text-muted text-xs">
				{/* The outcome stays on the row even though most will read
				    the same: `reconciled` quarantines a fork the user edited,
				    which is a materially different action from `migrated` and
				    must not be averaged away by the summary. */}
				{t(`skillRepairOutcome_${done ? "done_" : ""}${row.outcome}`)}
				{linkCount > 0 &&
					` · ${t("skillLayoutRowLinks", { count: linkCount })}`}
			</span>
		</li>
	);
}
