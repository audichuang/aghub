import { ArrowPathIcon } from "@heroicons/react/24/outline";
import { Alert, Button, Checkbox, Modal, Spinner, toast } from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { RepairReportDto } from "../generated/dto";
import { useApi } from "../hooks/use-api";
import {
	isBlocked,
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
	// `null` = everything, which is also what a fresh dialog shows. Kept as a
	// set of NAMES rather than indices so it survives the preview refetching
	// underneath (a skill migrated elsewhere simply drops out).
	const [picked, setPicked] = useState<Set<string> | null>(null);

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

	const result = repair.data;
	const shown: RepairReportDto[] = result ? result.skills : rows;
	const done = result ? !result.dry_run : false;
	// A blocked row is not something the user can choose to migrate, so it is
	// never selectable and never counted in the button.
	const selectable = useMemo(
		() => shown.filter((r) => !isBlocked(r)).map((r) => r.name),
		[shown],
	);
	const isPicked = (name: string) => picked === null || picked.has(name);
	const pickedNames = selectable.filter(isPicked);
	// The summary describes what the BUTTON will do, so before a run it counts
	// only the selected rows; after one it describes what happened to all.
	const summary = migrationSummary(
		done ? shown : shown.filter((r) => isBlocked(r) || isPicked(r.name)),
	);

	if (!visible) {
		return null;
	}

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
							onPress={() => {
								// The dialog reads the last run's rows when
								// there are any, so without this a re-open
								// shows the PREVIOUS result — "3 migrated" —
								// instead of the fresh preview of what is
								// still left. Only visible once a partial run
								// became possible.
								repair.reset();
								setPicked(null);
								setIsOpen(true);
							}}
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
								{summary.refused > 0 && (
									// Never buried: these are the rows the user
									// still has to act on, and the list below
									// may be scrolled past them.
									<p className="text-danger">
										{t("skillLayoutSummaryBlocked", {
											count: summary.refused,
										})}
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
										// Only offered BEFORE a run: after one,
										// a checkbox would suggest the finished
										// rows can still be chosen.
										picked={
											done ? null : isPicked(row.name)
										}
										onToggle={() =>
											setPicked((prev) => {
												const next = new Set(
													prev ?? selectable,
												);
												if (next.has(row.name)) {
													next.delete(row.name);
												} else {
													next.add(row.name);
												}
												return next;
											})
										}
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
										// Everything selected stays ONE bulk
										// request — the common case must not
										// become fifty round trips just
										// because the dialog can now narrow.
										names:
											pickedNames.length ===
											selectable.length
												? undefined
												: pickedNames,
										dryRun: false,
									})
								}
								isDisabled={
									repair.isPending ||
									(!done && pickedNames.length === 0)
								}
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
											: "skillLayoutApplyN",
										{ count: pickedNames.length },
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
function MigrationRow({
	row,
	done,
	picked,
	onToggle,
}: {
	row: RepairReportDto;
	done: boolean;
	/** `null` = not selectable (a finished run, or a blocked row). */
	picked: boolean | null;
	onToggle: () => void;
}) {
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
						{t(
							`skillRepairOutcome_${done ? "done_" : ""}${row.outcome}`,
						)}
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
		<li className="flex items-center gap-3 px-1 py-1 text-sm">
			{picked !== null && (
				/* Compound children with no label slot, and the root width
				   pinned: a bare `<Checkbox aria-label>` renders a ~35px root
				   that reserves room for a label, which throws the name column
				   out of line with the refused rows above it. Desktop
				   AGENTS.md names this trap. */
				<Checkbox
					aria-label={row.name}
					className="size-4 shrink-0"
					isSelected={picked}
					onChange={onToggle}
				>
					<Checkbox.Control>
						<Checkbox.Indicator />
					</Checkbox.Control>
				</Checkbox>
			)}
			<span className="min-w-0 flex-1 truncate">{row.name}</span>
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
