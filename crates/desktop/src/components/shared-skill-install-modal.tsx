import { Button, Modal } from "@heroui/react";
import { useTranslation } from "react-i18next";
import type { InstallResult } from "../lib/install-utils";
import type { Project } from "../lib/store";
import { InstallTargetSelector } from "./install-target-selector";
import { ResultStatusItem } from "./result-status-item";
import { SkillInfoCard } from "./skill-info-card";

export interface SharedSkillInstallModalProps {
	isOpen: boolean;
	onClose: () => void;
	/** Modal heading; defaults to t("installSkill") */
	heading?: string;
	/**
	 * Optional skill info card shown above the agent picker.
	 * Rendered whenever `source` is present; `name` is optional so callers
	 * can show the source row while omitting the skill name (e.g. installAll).
	 */
	skillInfo?: { source: string; name?: string } | null;
	/** The "select agents" body — rendered when installResults is empty */
	agentPickerSlot: React.ReactNode;
	/** When truthy, the results phase replaces the picker */
	installResults: InstallResult[];
	isInstalling: boolean;
	/**
	 * When false, InstallTargetSelector is not rendered at all.
	 * Defaults to true. Pass false for callers (e.g. ManageSkillAgentsDialog)
	 * that derive scope from the skill's own location and should not show the
	 * project/scope picker.
	 *
	 * Note: canInstallToProject=false does NOT hide InstallTargetSelector —
	 * it only disables the checkbox. Use this prop to hide it entirely.
	 */
	showTargetSelector?: boolean;
	/** Project/scope target selector */
	installToProject: boolean;
	canInstallToProject: boolean;
	onInstallToProjectChange: (v: boolean) => void;
	selectedProjectId: string | null;
	onSelectedProjectIdChange: (id: string | null) => void;
	projects: Project[];
	/** Confirm button label; defaults to t("install") */
	confirmLabel?: string;
	/** Confirm button disabled predicate (in addition to isInstalling) */
	isConfirmDisabled?: boolean;
	onConfirm: () => void;
	/** Anything extra to render in the picker body (e.g. "install all" checkbox) */
	extraPickerSlot?: React.ReactNode;
}

export function SharedSkillInstallModal({
	isOpen,
	onClose,
	heading,
	skillInfo,
	agentPickerSlot,
	installResults,
	isInstalling,
	showTargetSelector = true,
	installToProject,
	canInstallToProject,
	onInstallToProjectChange,
	selectedProjectId,
	onSelectedProjectIdChange,
	projects,
	confirmLabel,
	isConfirmDisabled = false,
	onConfirm,
	extraPickerSlot,
}: SharedSkillInstallModalProps) {
	const { t } = useTranslation();
	const isResultsPhase = installResults.length > 0;

	return (
		<Modal.Backdrop isOpen={isOpen} onOpenChange={onClose}>
			<Modal.Container>
				<Modal.Dialog className="w-[calc(100vw-2rem)] max-w-md sm:max-w-lg">
					<Modal.CloseTrigger />
					<Modal.Header>
						<Modal.Heading>
							{heading ?? t("installSkill")}
						</Modal.Heading>
					</Modal.Header>

					<Modal.Body className="p-4">
						<div className="space-y-4">
							{skillInfo?.source && (
								<SkillInfoCard
									name={skillInfo.name}
									source={skillInfo.source}
									className="mb-0"
								/>
							)}

							{!isResultsPhase && (
								<>
									<p className="text-sm text-muted">
										{t("selectAgentsForSkill")}
									</p>
									{agentPickerSlot}
									{extraPickerSlot}
									{showTargetSelector && (
										<InstallTargetSelector
											installToProject={installToProject}
											onInstallToProjectChange={
												onInstallToProjectChange
											}
											selectedProjectId={selectedProjectId}
											onSelectedProjectIdChange={
												onSelectedProjectIdChange
											}
											projects={projects}
											canInstallToProject={
												canInstallToProject
											}
										/>
									)}
								</>
							)}

							{isResultsPhase && (
								<div className="space-y-3">
									{installResults.map((result) => (
										<ResultStatusItem
											key={result.agentId}
											displayName={result.displayName}
											status={result.status}
											statusText={
												result.status === "pending"
													? t("installing")
													: result.status === "success"
														? t("installSuccess")
														: ""
											}
											error={result.error}
										/>
									))}
								</div>
							)}
						</div>
					</Modal.Body>

					<Modal.Footer>
						{!isResultsPhase && (
							<>
								<Button
									slot="close"
									variant="secondary"
									isDisabled={isInstalling}
								>
									{t("cancel")}
								</Button>
								<Button
									onPress={onConfirm}
									isDisabled={
										isConfirmDisabled || isInstalling
									}
								>
									{confirmLabel ??
										(isInstalling
											? t("installing")
											: t("install"))}
								</Button>
							</>
						)}
						{isResultsPhase && (
							<Button slot="close" variant="secondary">
								{t("done")}
							</Button>
						)}
					</Modal.Footer>
				</Modal.Dialog>
			</Modal.Container>
		</Modal.Backdrop>
	);
}
