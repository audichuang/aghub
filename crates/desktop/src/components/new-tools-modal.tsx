import { Button, Modal, Spinner } from "@heroui/react";
import { useTranslation } from "react-i18next";

interface NewToolsModalProps {
	isOpen: boolean;
	agentLabels: string[];
	skillCount: number;
	isLinking: boolean;
	onSkip: () => void;
	onLink: () => void;
}

export function NewToolsModal({
	isOpen,
	agentLabels,
	skillCount,
	isLinking,
	onSkip,
	onLink,
}: NewToolsModalProps) {
	const { t } = useTranslation();

	return (
		<Modal.Backdrop
			isOpen={isOpen}
			onOpenChange={(open) => {
				if (!open && !isLinking) onSkip();
			}}
		>
			<Modal.Container>
				<Modal.Dialog className="sm:max-w-[480px]">
					<Modal.Header>
						<Modal.Heading>{t("newToolsTitle")}</Modal.Heading>
					</Modal.Header>
					<Modal.Body className="grid gap-3 p-4">
						<p className="text-sm text-muted">
							{t("newToolsBody", {
								agents: agentLabels.join(", "),
								count: skillCount,
							})}
						</p>
						<ul className="list-inside list-disc text-sm text-foreground">
							{agentLabels.map((label) => (
								<li key={label}>{label}</li>
							))}
						</ul>
					</Modal.Body>
					<Modal.Footer>
						<Button
							variant="secondary"
							onPress={onSkip}
							isDisabled={isLinking}
						>
							{t("newToolsSkip")}
						</Button>
						<Button
							onPress={onLink}
							isDisabled={isLinking || agentLabels.length === 0}
						>
							{isLinking ? (
								<Spinner size="sm" color="current" />
							) : (
								t("newToolsLink")
							)}
						</Button>
					</Modal.Footer>
				</Modal.Dialog>
			</Modal.Container>
		</Modal.Backdrop>
	);
}
