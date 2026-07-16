import { Button, Modal } from "@heroui/react";
import { useTranslation } from "react-i18next";
import type { InstallResult } from "../lib/install-utils";
import type { Project } from "../lib/store";
import { InstallTargetSelector } from "./install-target-selector";
import { ResultStatusItem } from "./result-status-item";
import { SkillInfoCard } from "./skill-info-card";

/**
 * Props for the target-selector section.
 * All required when showTargetSelector is true; omitted entirely when false.
 */
interface TargetSelectorProps {
	installToProject: boolean;
	canInstallToProject: boolean;
	onInstallToProjectChange: (v: boolean) => void;
	selectedProjectId: string | null;
	onSelectedProjectIdChange: (id: string | null) => void;
	projects: Project[];
}

export type SharedSkillInstallModalProps = {
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
	/**
	 * Optional prompt rendered above the agent picker. When omitted, no prompt
	 * is shown (e.g. ManageSkillAgentsDialog already has its own label inside
	 * SkillsAgentList). Pass a React node from the skills-sh caller to restore
	 * the "Select agents for this skill" copy.
	 */
	descriptionSlot?: React.ReactNode;
	/** When truthy, the results phase replaces the picker */
	installResults: InstallResult[];
	isInstalling: boolean;
	/**
	 * When false, InstallTargetSelector is not rendered at all and the
	 * target-selector props below are NOT required.
	 * Defaults to true. Pass false for callers (e.g. ManageSkillAgentsDialog)
	 * that derive scope from the skill's own location and should not show the
	 * project/scope picker.
	 *
	 * Note: canInstallToProject=false does NOT hide InstallTargetSelector —
	 * it only disables the checkbox. Use this prop to hide it entirely.
	 */
	showTargetSelector?: boolean;
	/** Confirm button label; defaults to t("install") */
	confirmLabel?: string;
	/** Confirm button disabled predicate (in addition to isInstalling) */
	isConfirmDisabled?: boolean;
	onConfirm: () => void;
	/** Anything extra to render in the picker body (e.g. "install all" checkbox) */
	extraPickerSlot?: React.ReactNode;
} & (
	| { showTargetSelector: false }
	| ({ showTargetSelector?: true } & TargetSelectorProps)
);

export function SharedSkillInstallModal(props: SharedSkillInstallModalProps) {
	const { t } = useTranslation();
	const {
		isOpen,
		onClose,
		heading,
		skillInfo,
		agentPickerSlot,
		descriptionSlot,
		installResults,
		isInstalling,
		showTargetSelector = true,
		confirmLabel,
		isConfirmDisabled = false,
		onConfirm,
		extraPickerSlot,
	} = props;

	const isResultsPhase = installResults.length > 0;

	// Extract target-selector props only when showTargetSelector is true
	const targetSelectorProps =
		showTargetSelector !== false
			? {
					installToProject: (props as TargetSelectorProps)
						.installToProject,
					canInstallToProject: (props as TargetSelectorProps)
						.canInstallToProject,
					onInstallToProjectChange: (props as TargetSelectorProps)
						.onInstallToProjectChange,
					selectedProjectId: (props as TargetSelectorProps)
						.selectedProjectId,
					onSelectedProjectIdChange: (props as TargetSelectorProps)
						.onSelectedProjectIdChange,
					projects: (props as TargetSelectorProps).projects,
				}
			: null;

	return (
		<Modal.Backdrop isOpen={isOpen} onOpenChange={onClose}>
			<Modal.Container>
				<Modal.Dialog className="flex max-h-[85vh] w-[calc(100vw-2rem)] max-w-md flex-col overflow-hidden sm:max-w-lg">
					<Modal.CloseTrigger />
					<Modal.Header>
						<Modal.Heading>
							{heading ?? t("installSkill")}
						</Modal.Heading>
					</Modal.Header>

					<Modal.Body className="flex min-h-0 flex-1 flex-col p-0">
						<div className="flex-1 space-y-4 overflow-y-auto p-4">
							{skillInfo?.source && (
								<SkillInfoCard
									name={skillInfo.name}
									source={skillInfo.source}
									className="mb-0"
								/>
							)}

							{!isResultsPhase && (
								<>
									{descriptionSlot}
									{agentPickerSlot}
									{extraPickerSlot}
									{showTargetSelector &&
										targetSelectorProps && (
											<InstallTargetSelector
												installToProject={
													targetSelectorProps.installToProject
												}
												onInstallToProjectChange={
													targetSelectorProps.onInstallToProjectChange
												}
												selectedProjectId={
													targetSelectorProps.selectedProjectId
												}
												onSelectedProjectIdChange={
													targetSelectorProps.onSelectedProjectIdChange
												}
												projects={
													targetSelectorProps.projects
												}
												canInstallToProject={
													targetSelectorProps.canInstallToProject
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
													: result.status ===
														  "success"
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
