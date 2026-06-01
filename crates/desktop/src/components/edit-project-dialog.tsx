import { FolderIcon } from "@heroicons/react/24/outline";
import {
	Button,
	FieldError,
	Fieldset,
	Form,
	Input,
	Label,
	Modal,
	TextField,
} from "@heroui/react";
import { open } from "@tauri-apps/plugin-dialog";
import { basename } from "pathe";
import { useState } from "react";
import { Controller, useForm } from "react-hook-form";
import { useTranslation } from "react-i18next";
import { useConnection } from "../hooks/use-connection";
import { useAddProject } from "../hooks/use-projects";
import { LOCAL_CONNECTION } from "../lib/connection-logic";
import { RemoteDirectoryPickerDialog } from "./remote-directory-picker-dialog";

interface CreateProjectDialogProps {
	isOpen: boolean;
	onClose: () => void;
}

interface CreateProjectFormValues {
	name: string;
	path: string;
}

export function CreateProjectDialog({
	isOpen,
	onClose,
}: CreateProjectDialogProps) {
	const { t } = useTranslation();
	const { activeConnection } = useConnection();
	const addProject = useAddProject();
	const isLocalConnection = activeConnection.id === LOCAL_CONNECTION.id;
	const [isRemotePickerOpen, setIsRemotePickerOpen] = useState(false);
	const {
		control,
		handleSubmit,
		getValues,
		setValue,
		reset,
		formState: { isSubmitting },
	} = useForm<CreateProjectFormValues>({
		mode: "onSubmit",
		reValidateMode: "onChange",
		defaultValues: {
			name: "",
			path: "",
		},
	});

	const fillNameFromPath = (path: string) => {
		const trimmedPath = path.trim().replace(/[/\\]+$/, "");
		const folderName = basename(trimmedPath) || "";
		if (folderName && !getValues("name").trim()) {
			setValue("name", folderName, {
				shouldDirty: true,
				shouldValidate: true,
			});
		}
	};

	const handleFolderSelect = async () => {
		try {
			const selectedPath = await open({
				directory: true,
				multiple: false,
			});
			if (selectedPath) {
				setValue("path", selectedPath, {
					shouldDirty: true,
					shouldValidate: true,
				});
				fillNameFromPath(selectedPath);
			}
		} catch (error) {
			console.error("Failed to pick folder:", error);
		}
	};

	const handleSave = (values: CreateProjectFormValues) => {
		addProject.mutate(
			{ name: values.name.trim(), path: values.path.trim() },
			{
				onSuccess: () => {
					reset();
					onClose();
				},
				onError: (error) => {
					console.error("Failed to create project:", error);
				},
			},
		);
	};

	return (
		<>
			<Modal.Backdrop isOpen={isOpen} onOpenChange={onClose}>
				<Modal.Container>
					<Modal.Dialog>
						<Modal.CloseTrigger />
						<Form
							validationBehavior="aria"
							onSubmit={handleSubmit(handleSave)}
						>
							<Modal.Header>
								<Modal.Heading>{t("addProject")}</Modal.Heading>
							</Modal.Header>
							<Modal.Body className="p-2">
								<Fieldset>
									<Controller
										name="path"
										control={control}
										rules={{
											required: t(
												"validationProjectPathRequired",
											),
											validate: (value) =>
												value.trim()
													? true
													: t(
															"validationProjectPathRequired",
														),
										}}
										render={({ field, fieldState }) => (
											<div className="flex flex-col gap-2">
												{isLocalConnection ? (
													<>
														<Label>
															{t("projectPath")}
														</Label>
														<button
															type="button"
															onClick={
																handleFolderSelect
															}
															className="
           flex h-32 w-full cursor-pointer flex-col items-center justify-center
           rounded-lg border-2 border-dashed border-border bg-transparent
           transition-colors
           hover:bg-surface-secondary
         "
														>
															<FolderIcon className="mb-2 size-10 text-muted" />
															<span className="text-sm font-medium text-foreground">
																{t(
																	"selectProjectFolder",
																)}
															</span>
															<span className="text-xs text-muted">
																{t(
																	"clickToBrowse",
																)}
															</span>
														</button>
														{field.value && (
															<Input
																value={
																	field.value
																}
																readOnly
																className="mt-2"
																variant="secondary"
															/>
														)}
														{fieldState.error && (
															<FieldError>
																{
																	fieldState
																		.error
																		.message
																}
															</FieldError>
														)}
													</>
												) : (
													<div className="flex items-end gap-2">
														<TextField
															className="min-w-0 flex-1"
															variant="secondary"
															isRequired
															validationBehavior="aria"
															isInvalid={Boolean(
																fieldState.error,
															)}
														>
															<Label>
																{t(
																	"projectPath",
																)}
															</Label>
															<Input
																value={
																	field.value
																}
																onChange={(e) =>
																	field.onChange(
																		e.target
																			.value,
																	)
																}
																onBlur={(e) => {
																	field.onBlur();
																	fillNameFromPath(
																		e.target
																			.value,
																	);
																}}
																placeholder={t(
																	"remoteProjectPathPlaceholder",
																)}
																variant="secondary"
															/>
															{fieldState.error && (
																<FieldError>
																	{
																		fieldState
																			.error
																			.message
																	}
																</FieldError>
															)}
														</TextField>
														<Button
															type="button"
															variant="secondary"
															onPress={() =>
																setIsRemotePickerOpen(
																	true,
																)
															}
														>
															{t(
																"browseRemoteDirectory",
															)}
														</Button>
													</div>
												)}
											</div>
										)}
									/>

									<Controller
										name="name"
										control={control}
										rules={{
											required: t(
												"validationProjectNameRequired",
											),
											validate: (value) =>
												value.trim()
													? true
													: t(
															"validationProjectNameRequired",
														),
										}}
										render={({ field, fieldState }) => (
											<TextField
												className="w-full"
												variant="secondary"
												isRequired
												validationBehavior="aria"
												isInvalid={Boolean(
													fieldState.error,
												)}
											>
												<Label>
													{t("projectName")}
												</Label>
												<Input
													value={field.value}
													onChange={(e) =>
														field.onChange(
															e.target.value,
														)
													}
													onBlur={field.onBlur}
													placeholder={t(
														"projectName",
													)}
													variant="secondary"
												/>
												{fieldState.error && (
													<FieldError>
														{
															fieldState.error
																.message
														}
													</FieldError>
												)}
											</TextField>
										)}
									/>
								</Fieldset>
							</Modal.Body>
							<Modal.Footer>
								<Button
									type="button"
									slot="close"
									variant="secondary"
								>
									{t("cancel")}
								</Button>
								<Button
									type="submit"
									isDisabled={
										addProject.isPending || isSubmitting
									}
								>
									{t("create")}
								</Button>
							</Modal.Footer>
						</Form>
					</Modal.Dialog>
				</Modal.Container>
			</Modal.Backdrop>
			<RemoteDirectoryPickerDialog
				isOpen={isRemotePickerOpen}
				connection={activeConnection}
				initialPath={getValues("path")}
				onClose={() => setIsRemotePickerOpen(false)}
				onSelect={(path) => {
					setValue("path", path, {
						shouldDirty: true,
						shouldValidate: true,
					});
					fillNameFromPath(path);
				}}
			/>
		</>
	);
}
