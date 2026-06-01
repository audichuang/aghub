import {
	ArrowPathIcon,
	FolderIcon,
	FolderOpenIcon,
} from "@heroicons/react/24/outline";
import {
	Button,
	FieldError,
	Form,
	Input,
	Label,
	ListBox,
	Modal,
	Spinner,
	TextField,
} from "@heroui/react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Selection } from "react-aria-components";
import type { Connection } from "../lib/store";

interface RemoteDirectoryEntry {
	name: string;
	path: string;
}

interface RemoteDirectoryListing {
	path: string;
	entries: RemoteDirectoryEntry[];
}

interface RemoteDirectoryPickerDialogProps {
	isOpen: boolean;
	connection: Connection;
	initialPath: string;
	onClose: () => void;
	onSelect: (path: string) => void;
}

interface RemoteErrorPayload {
	kind?: string;
	stderr?: string;
	message?: string;
}

function remoteErrorMessage(error: unknown): string {
	if (error instanceof Error) {
		return error.message;
	}
	if (typeof error === "string") {
		return error;
	}
	if (error == null || typeof error !== "object") {
		return String(error);
	}

	const remote = error as RemoteErrorPayload;
	if (remote.stderr) {
		return remote.stderr;
	}
	if (remote.message) {
		return remote.message;
	}
	try {
		return JSON.stringify(error);
	} catch {
		return String(error);
	}
}

function normalizedInitialPath(path: string): string {
	const trimmed = path.trim();
	return trimmed.length > 0 ? trimmed : "~";
}

export function RemoteDirectoryPickerDialog({
	isOpen,
	connection,
	initialPath,
	onClose,
	onSelect,
}: RemoteDirectoryPickerDialogProps) {
	const { t } = useTranslation();
	const [pathInput, setPathInput] = useState(() =>
		normalizedInitialPath(initialPath),
	);
	const [browsePath, setBrowsePath] = useState(() =>
		normalizedInitialPath(initialPath),
	);

	useEffect(() => {
		if (!isOpen) return;
		const nextPath = normalizedInitialPath(initialPath);
		setPathInput(nextPath);
		setBrowsePath(nextPath);
	}, [initialPath, isOpen]);

	const directoryQuery = useQuery({
		queryKey: ["remote-directories", connection.id, browsePath],
		queryFn: () =>
			invoke<RemoteDirectoryListing>("list_remote_directories", {
				connection,
				path: browsePath,
			}),
		enabled: isOpen,
		retry: false,
	});

	useEffect(() => {
		if (directoryQuery.data?.path) {
			setPathInput(directoryQuery.data.path);
		}
	}, [directoryQuery.data?.path]);

	const goToPath = (path: string) => {
		const nextPath = normalizedInitialPath(path);
		setPathInput(nextPath);
		setBrowsePath(nextPath);
	};

	const handleSelectionChange = (keys: Selection) => {
		if (keys === "all") return;
		const key = [...keys][0] as string | undefined;
		if (!key) return;
		goToPath(key);
	};

	const handleSelectCurrent = () => {
		const selectedPath = directoryQuery.data?.path ?? pathInput.trim();
		if (!selectedPath) return;
		onSelect(selectedPath);
		onClose();
	};

	const errorMessage = directoryQuery.error
		? remoteErrorMessage(directoryQuery.error)
		: null;
	const entries = directoryQuery.data?.entries ?? [];

	return (
		<Modal.Backdrop isOpen={isOpen} onOpenChange={onClose}>
			<Modal.Container>
				<Modal.Dialog>
					<Modal.CloseTrigger />
					<Modal.Header>
						<Modal.Heading>
							{t("remoteDirectoryPickerTitle")}
						</Modal.Heading>
					</Modal.Header>
					<Modal.Body className="p-2">
						<Form
							validationBehavior="aria"
							onSubmit={(event) => {
								event.preventDefault();
								goToPath(pathInput);
							}}
						>
							<div className="flex w-full items-end gap-2">
								<TextField
									className="min-w-0 flex-1"
									variant="secondary"
									validationBehavior="aria"
									isInvalid={Boolean(errorMessage)}
								>
									<Label>{t("projectPath")}</Label>
									<Input
										value={pathInput}
										onChange={(event) =>
											setPathInput(event.target.value)
										}
										placeholder={t(
											"remoteProjectPathPlaceholder",
										)}
										variant="secondary"
									/>
									{errorMessage && (
										<FieldError>{errorMessage}</FieldError>
									)}
								</TextField>
								<Button
									type="submit"
									variant="secondary"
									isDisabled={directoryQuery.isFetching}
								>
									{t("goToPath")}
								</Button>
							</div>
						</Form>

						<div className="mt-3 h-72 overflow-auto rounded-md border border-border">
							{directoryQuery.isLoading ? (
								<div className="flex h-full items-center justify-center">
									<Spinner />
								</div>
							) : entries.length === 0 ? (
								<div className="flex h-full flex-col items-center justify-center gap-2 text-muted">
									<FolderOpenIcon className="size-8" />
									<p className="text-sm">
										{errorMessage
											? t("remoteDirectoryLoadFailed")
											: t("remoteDirectoryEmpty")}
									</p>
								</div>
							) : (
								<ListBox
									aria-label={t("remoteDirectoryPickerTitle")}
									selectionMode="single"
									selectionBehavior="replace"
									selectedKeys={new Set()}
									onSelectionChange={handleSelectionChange}
									className="p-2"
								>
									{entries.map((entry) => (
										<ListBox.Item
											key={entry.path}
											id={entry.path}
											textValue={entry.name}
											className="data-hovered:bg-surface-secondary"
										>
											<div className="flex min-w-0 items-center gap-2">
												<FolderIcon className="size-4 shrink-0 text-muted" />
												<span className="truncate text-sm">
													{entry.name === ".."
														? t("parentDirectory")
														: entry.name}
												</span>
												<span className="truncate text-xs text-muted">
													{entry.path}
												</span>
											</div>
										</ListBox.Item>
									))}
								</ListBox>
							)}
						</div>
					</Modal.Body>
					<Modal.Footer>
						<Button type="button" slot="close" variant="secondary">
							{t("cancel")}
						</Button>
						<Button
							type="button"
							isIconOnly
							variant="secondary"
							aria-label={t("refreshRemoteDirectories")}
							isDisabled={directoryQuery.isFetching}
							onPress={() => void directoryQuery.refetch()}
						>
							<ArrowPathIcon className="size-4" />
						</Button>
						<Button
							type="button"
							isDisabled={!directoryQuery.data?.path}
							onPress={handleSelectCurrent}
						>
							{t("selectCurrentDirectory")}
						</Button>
					</Modal.Footer>
				</Modal.Dialog>
			</Modal.Container>
		</Modal.Backdrop>
	);
}
