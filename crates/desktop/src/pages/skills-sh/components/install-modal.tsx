import { Checkbox, Label } from "@heroui/react";
import { useTranslation } from "react-i18next";
import { AgentSelector } from "../../../components/agent-selector";
import { SharedSkillInstallModal } from "../../../components/shared-skill-install-modal";
import type { MarketSkill } from "../../../generated/dto";
import type { InstallResult } from "../../../lib/install-utils";
import type { Project } from "../../../lib/store";

interface InstallModalProps {
	isOpen: boolean;
	selectedSkill: MarketSkill | null;
	selectedAgents: Set<string>;
	onSelectedAgentsChange: (agents: Set<string>) => void;
	installResults: InstallResult[];
	isInstalling: boolean;
	skillAgents: ReturnType<
		typeof import("../hooks/use-skill-install").useSkillInstall
	>["skillAgents"];
	installAll: boolean;
	onInstallAllChange: (value: boolean) => void;
	installToProject: boolean;
	canInstallToProject: boolean;
	onInstallToProjectChange: (value: boolean) => void;
	selectedProjectId: string | null;
	onSelectedProjectIdChange: (id: string | null) => void;
	projects: Project[];
	onClose: () => void;
	onInstall: () => void;
}

export function InstallModal({
	isOpen,
	selectedSkill,
	selectedAgents,
	onSelectedAgentsChange,
	installResults,
	isInstalling,
	skillAgents,
	installAll,
	onInstallAllChange,
	installToProject,
	canInstallToProject,
	onInstallToProjectChange,
	selectedProjectId,
	onSelectedProjectIdChange,
	projects,
	onClose,
	onInstall,
}: InstallModalProps) {
	const { t } = useTranslation();

	const agentPicker = (
		<AgentSelector
			agents={skillAgents}
			selectedKeys={selectedAgents}
			onSelectionChange={onSelectedAgentsChange}
			emptyMessage={t("noTargetAgents")}
			showSelectedIcon
			variant="secondary"
		/>
	);

	const installAllCheckbox = (
		<Checkbox
			value="installAll"
			isSelected={installAll}
			onChange={(isSelected) => onInstallAllChange(isSelected)}
			variant="secondary"
		>
			<Checkbox.Control>
				<Checkbox.Indicator />
			</Checkbox.Control>
			<Checkbox.Content className="flex flex-col items-start gap-0.5">
				<Label className="text-sm font-medium">
					{t("installAllSkills")}
				</Label>
				<span className="text-xs text-muted">
					{t("installAllSkillsDescription")}
				</span>
			</Checkbox.Content>
		</Checkbox>
	);

	return (
		<SharedSkillInstallModal
			isOpen={isOpen}
			onClose={onClose}
			skillInfo={
				selectedSkill
					? {
							source: selectedSkill.source,
							name: installAll ? undefined : selectedSkill.name,
						}
					: null
			}
			agentPickerSlot={agentPicker}
			extraPickerSlot={installAllCheckbox}
			installResults={installResults}
			isInstalling={isInstalling}
			installToProject={installToProject}
			canInstallToProject={canInstallToProject}
			onInstallToProjectChange={onInstallToProjectChange}
			selectedProjectId={selectedProjectId}
			onSelectedProjectIdChange={onSelectedProjectIdChange}
			projects={projects}
			isConfirmDisabled={
				selectedAgents.size === 0 ||
				(installToProject && !selectedProjectId)
			}
			onConfirm={onInstall}
		/>
	);
}
