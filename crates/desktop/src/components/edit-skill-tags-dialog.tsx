import { XMarkIcon } from "@heroicons/react/24/solid";
import { Button, Input, Modal, TextField } from "@heroui/react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useSkillTags } from "../hooks/use-skill-tags";
import { normalizeTag, unionTags } from "../lib/skill-tags";

interface EditSkillTagsDialogProps {
	isOpen: boolean;
	/** One skill, or the whole multi-select. The dialog behaves identically:
	 * typing adds a tag to every name, an X removes it from every name. */
	names: string[];
	onClose: () => void;
}

export function EditSkillTagsDialog({
	isOpen,
	names,
	onClose,
}: EditSkillTagsDialogProps) {
	const { t } = useTranslation();
	const { tags, applyTag } = useSkillTags();
	const [draft, setDraft] = useState("");

	const present = unionTags(tags, names);
	const canAdd = normalizeTag(draft) !== null && names.length > 0;

	const add = () => {
		if (!canAdd) return;
		void applyTag(names, "add", draft);
		setDraft("");
	};

	return (
		<Modal.Backdrop
			isOpen={isOpen}
			onOpenChange={(open) => {
				if (!open) {
					setDraft("");
					onClose();
				}
			}}
		>
			<Modal.Container>
				<Modal.Dialog className="sm:max-w-[420px]">
					<Modal.Header>
						<Modal.Heading>
							{names.length > 1
								? t("editTagsForSelection", {
										count: names.length,
									})
								: t("editTagsFor", { name: names[0] ?? "" })}
						</Modal.Heading>
					</Modal.Header>
					<Modal.Body className="grid gap-3 p-4">
						<div className="flex flex-wrap gap-2">
							{present.length === 0 ? (
								<span className="text-sm text-muted">
									{t("noTagsYet")}
								</span>
							) : (
								present.map((tag) => (
									<span
										key={tag}
										className="inline-flex items-center gap-1 rounded-full bg-surface-secondary px-2 py-0.5 text-xs text-foreground"
									>
										{tag}
										<button
											type="button"
											aria-label={t("removeTag", { tag })}
											className="text-muted hover:text-danger"
											onClick={() =>
												void applyTag(
													names,
													"remove",
													tag,
												)
											}
										>
											<XMarkIcon className="size-3" />
										</button>
									</span>
								))
							)}
						</div>
						<TextField className="w-full" aria-label={t("addTag")}>
							<Input
								value={draft}
								variant="secondary"
								placeholder={t("addTagPlaceholder")}
								onChange={(e) => setDraft(e.target.value)}
								onKeyDown={(e) => {
									if (e.key !== "Enter") return;
									e.preventDefault();
									add();
								}}
							/>
						</TextField>
					</Modal.Body>
					<Modal.Footer>
						<Button variant="secondary" onPress={onClose}>
							{t("close")}
						</Button>
						<Button onPress={add} isDisabled={!canAdd}>
							{t("addTag")}
						</Button>
					</Modal.Footer>
				</Modal.Dialog>
			</Modal.Container>
		</Modal.Backdrop>
	);
}
