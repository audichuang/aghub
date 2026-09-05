import { ArrowPathIcon } from "@heroicons/react/24/outline";
import { Alert, Button, Modal, Spinner, toast } from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { RepairReportDto } from "../generated/dto";
import { useApi } from "../hooks/use-api";
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
		}),
	});

	// `isSuccess`, not `!isLoading`: a FAILED query settles with `data`
	// undefined, and `?? []` would render that as "nothing to migrate" —
	// indistinguishable from a real all-clear.
	const rows = isSuccess ? data.skills : [];
	if (rows.length === 0) {
		return null;
	}

	const result = repair.data;
	const shown: RepairReportDto[] = result ? result.skills : rows;

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
					<Modal.Dialog>
						<Modal.CloseTrigger />
						<Modal.Header>
							<div className="flex items-center gap-2">
								<ArrowPathIcon className="size-5 text-warning" />
								<Modal.Heading>
									{t("skillLayoutMigrateTitle")}
								</Modal.Heading>
							</div>
						</Modal.Header>
						<Modal.Body>
							<p className="mb-4 text-sm text-muted">
								{t("skillLayoutMigrateExplain")}
							</p>
							<ul className="space-y-4">
								{shown.map((row) => (
									<MigrationRow key={row.name} row={row} />
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
								{t("close")}
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
 * One skill's row. Shows the three things the preview has to answer: where the
 * master goes, which per-agent links appear, and who stays fused afterwards.
 */
function MigrationRow({ row }: { row: RepairReportDto }) {
	const { t } = useTranslation();
	const refused = row.outcome === "refused";
	return (
		<li className="rounded-md border border-default p-3 text-sm">
			<div className="flex items-center justify-between gap-2">
				<span className="font-medium">{row.name}</span>
				<span
					className={
						refused ? "text-danger text-xs" : "text-muted text-xs"
					}
				>
					{t(`skillRepairOutcome_${row.outcome}`)}
				</span>
			</div>
			{refused ? (
				<div className="mt-2 space-y-1">
					<p className="text-danger text-xs">{row.reason}</p>
					{/* Shown verbatim and selectable: it is the literal command
					    that unsticks the user. Paraphrasing it would leave them
					    with a diagnosis and no way out. */}
					<pre className="overflow-x-auto rounded bg-muted p-2 text-xs select-text">
						{row.fix}
					</pre>
				</div>
			) : (
				<div className="mt-2 space-y-1 text-muted text-xs">
					<p className="break-all">
						{t("skillLayoutMasterMovesTo", { path: row.master })}
					</p>
					{row.referrers.length > 0 && (
						<p>
							{t("skillLayoutNewLinks", {
								count: row.referrers.length,
							})}
						</p>
					)}
					{row.fused.length > 0 && (
						// The honest half. These agents do NOT become
						// individually revocable, and saying so is the
						// difference between a migration the user understands
						// and one they merely trust.
						<p>
							{t("skillLayoutStillShared", {
								agents: row.fused.join(", "),
							})}
						</p>
					)}
				</div>
			)}
		</li>
	);
}
