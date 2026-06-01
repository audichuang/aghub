import {
	CheckCircleIcon,
	ChevronDownIcon,
	ExclamationTriangleIcon,
	PencilSquareIcon,
	TrashIcon,
	XCircleIcon,
} from "@heroicons/react/24/solid";
import {
	Button,
	Description,
	Dropdown,
	FieldError,
	Header,
	Input,
	Label,
	Modal,
	NumberField,
	TextField,
	toast,
} from "@heroui/react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { type Key, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TestResult } from "../contexts/connection";
import { useConnection } from "../hooks/use-connection";
import { LOCAL_CONNECTION } from "../lib/connection-logic";
import {
	connectionToForm,
	type ConnectionFormState,
	EMPTY_CONNECTION_FORM,
	formToConnection,
	isFormValid,
	validateConnectionForm,
} from "../lib/connection-form";
import type { Connection } from "../lib/store";
import { cn } from "../lib/utils";

interface ManageConnectionsDialogProps {
	isOpen: boolean;
	onClose: () => void;
}

interface SshConfigHost {
	alias: string;
	hostName?: string | null;
}

type TestStepStatus = "ok" | "warning" | "error" | "muted";

interface TestStep {
	key: string;
	label: string;
	detail: string;
	status: TestStepStatus;
}

/** Format a TestResult into a translated toast message. */
function toastTestResult(
	result: TestResult,
	t: (key: string, opts?: Record<string, unknown>) => string,
): void {
	if (!result.reachable) {
		toast.danger(t("connTestUnreachable", { message: result.message }));
		return;
	}
	if (result.installAttempted && !result.installSucceeded) {
		toast.danger(
			t("connTestInstallFailed", {
				message: result.installMessage ?? result.message,
			}),
		);
		return;
	}
	if (!result.apiPresent) {
		toast.danger(t("connTestApiMissing", { message: result.message }));
		return;
	}
	if (!result.compatible) {
		toast.warning(
			t("connTestIncompatible", {
				version: result.apiVersion ?? "?",
				message: result.message,
			}),
		);
		return;
	}
	if (result.installAttempted && result.installSucceeded) {
		toast.success(t("connTestInstalledOk", { message: result.message }));
		return;
	}
	toast.success(t("connTestReachableOk", { message: result.message }));
}

function buildTestSteps(
	result: TestResult,
	t: (key: string, opts?: Record<string, unknown>) => string,
): TestStep[] {
	const apiStatus: TestStepStatus = !result.reachable
		? "muted"
		: result.apiPresent
			? result.compatible
				? "ok"
				: "warning"
			: "error";

	const apiDetail = !result.reachable
		? t("connTestStepSkipped")
		: result.apiPresent
			? result.apiVersion == null
				? t("connTestStepApiPresent")
				: t("connTestStepApiVersion", {
						version: result.apiVersion,
					})
			: t("connTestStepApiMissing");

	const installStatus: TestStepStatus = !result.reachable
		? "muted"
		: result.installAttempted
			? result.installSucceeded
				? "ok"
				: "error"
			: result.apiPresent
				? "muted"
				: "warning";

	const installDetail = !result.reachable
		? t("connTestStepSkipped")
		: result.installAttempted
			? result.installSucceeded
				? t("connTestStepInstallOk")
				: (result.installMessage ?? result.message)
			: result.apiPresent
				? t("connTestStepInstallNotNeeded")
				: t("connTestStepInstallNotRun");

	return [
		{
			key: "ssh",
			label: t("connTestStepSsh"),
			detail: result.reachable ? t("connTestStepSshOk") : result.message,
			status: result.reachable ? "ok" : "error",
		},
		{
			key: "api",
			label: t("connTestStepApi"),
			detail: apiDetail,
			status: apiStatus,
		},
		{
			key: "install",
			label: t("connTestStepInstall"),
			detail: installDetail,
			status: installStatus,
		},
	];
}

function TestStepIcon({ status }: { status: TestStepStatus }) {
	if (status === "ok") {
		return (
			<CheckCircleIcon className="mt-0.5 size-4 shrink-0 text-success" />
		);
	}
	if (status === "warning") {
		return (
			<ExclamationTriangleIcon className="mt-0.5 size-4 shrink-0 text-warning" />
		);
	}
	if (status === "error") {
		return <XCircleIcon className="mt-0.5 size-4 shrink-0 text-danger" />;
	}
	return (
		<span
			className="mt-1 size-3 shrink-0 rounded-full bg-muted/40"
			aria-hidden="true"
		/>
	);
}

function TestResultPanel({
	result,
	t,
}: {
	result: TestResult;
	t: (key: string, opts?: Record<string, unknown>) => string;
}) {
	const steps = buildTestSteps(result, t);

	return (
		<div className="rounded-md border border-border bg-surface-secondary px-3 py-2.5">
			<p className="text-sm font-medium text-foreground">
				{t("connTestResultTitle")}
			</p>
			<div className="mt-2 flex flex-col gap-2">
				{steps.map((step) => (
					<div key={step.key} className="flex min-w-0 gap-2">
						<TestStepIcon status={step.status} />
						<div className="min-w-0">
							<p className="text-sm font-medium text-foreground">
								{step.label}
							</p>
							<p className="break-words text-xs text-muted">
								{step.detail}
							</p>
						</div>
					</div>
				))}
			</div>
		</div>
	);
}

export function ManageConnectionsDialog({
	isOpen,
	onClose,
}: ManageConnectionsDialogProps) {
	const { t } = useTranslation();
	const {
		connections,
		addConnection,
		updateConnection,
		removeConnection,
		testConnection,
	} = useConnection();

	// Only user remotes are editable; the implicit Local connection is not.
	const remotes = useMemo(
		() => connections.filter((c) => c.id !== LOCAL_CONNECTION.id),
		[connections],
	);

	// The connection currently being edited, or null while adding a new one.
	const [editingId, setEditingId] = useState<string | null>(null);
	const [form, setForm] = useState<ConnectionFormState>(
		EMPTY_CONNECTION_FORM,
	);
	// Validation errors are only shown after a save/test attempt.
	const [showErrors, setShowErrors] = useState(false);
	const [testResult, setTestResult] = useState<TestResult | null>(null);

	const { data: sshConfigHosts = [], isPending: sshHostsLoading } = useQuery<
		SshConfigHost[]
	>({
		queryKey: ["ssh-config-hosts"],
		queryFn: () => invoke<SshConfigHost[]>("list_ssh_config_hosts"),
		enabled: isOpen,
	});

	const errors = useMemo(() => validateConnectionForm(form), [form]);
	const valid = isFormValid(form);

	const setField = (key: keyof ConnectionFormState, value: string) => {
		setTestResult(null);
		setForm((prev) => ({ ...prev, [key]: value }));
	};

	const resetForm = () => {
		setEditingId(null);
		setForm(EMPTY_CONNECTION_FORM);
		setShowErrors(false);
		setTestResult(null);
	};

	const selectSshAlias = (key: Key) => {
		if (typeof key !== "string") return;
		const host = sshConfigHosts.find(
			(candidate) => candidate.alias === key,
		);
		if (host != null) {
			setField("sshTarget", host.alias);
		}
	};

	const startEdit = (connection: Connection) => {
		setEditingId(connection.id);
		setForm(connectionToForm(connection));
		setShowErrors(false);
	};

	const saveMutation = useMutation({
		mutationFn: async () => {
			const payload = formToConnection(form);
			if (editingId !== null) {
				await updateConnection({ ...payload, id: editingId });
				return "update" as const;
			}
			await addConnection(payload);
			return "add" as const;
		},
		onSuccess: (kind) => {
			toast.success(t(kind === "update" ? "connUpdated" : "connAdded"));
			resetForm();
		},
		onError: (err) => {
			toast.danger(
				err instanceof Error ? err.message : t("connSaveError"),
			);
		},
	});

	const removeMutation = useMutation({
		mutationFn: (id: string) => removeConnection(id),
		onSuccess: (_data, id) => {
			toast.success(t("connRemoved"));
			if (editingId === id) {
				resetForm();
			}
		},
		onError: (err) => {
			toast.danger(
				err instanceof Error ? err.message : t("connRemoveError"),
			);
		},
	});

	const testMutation = useMutation({
		mutationFn: () => {
			const payload = formToConnection(form);
			return testConnection({ ...payload, id: editingId ?? "test" });
		},
		onSuccess: (result) => {
			setTestResult(result);
			toastTestResult(result, t);
		},
		onError: (err) => {
			setTestResult(null);
			toast.danger(
				t("connTestFailed", {
					message: err instanceof Error ? err.message : String(err),
				}),
			);
		},
	});

	const handleSave = () => {
		setShowErrors(true);
		if (!valid) return;
		saveMutation.mutate();
	};

	const handleTest = () => {
		setShowErrors(true);
		if (!valid) return;
		setTestResult(null);
		testMutation.mutate();
	};

	const isBusy =
		saveMutation.isPending ||
		removeMutation.isPending ||
		testMutation.isPending;

	const handleOpenChange = (open: boolean) => {
		if (!open) {
			resetForm();
			onClose();
		}
	};

	return (
		<Modal.Backdrop isOpen={isOpen} onOpenChange={handleOpenChange}>
			<Modal.Container>
				<Modal.Dialog className="w-[calc(100vw-2rem)] max-w-md sm:max-w-lg">
					<Modal.CloseTrigger />
					<Modal.Header>
						<Modal.Heading>{t("connManageTitle")}</Modal.Heading>
					</Modal.Header>

					<Modal.Body className="flex flex-col gap-5 p-4">
						<div className="flex flex-col gap-2">
							{remotes.length === 0 ? (
								<p className="text-sm text-muted">
									{t("connNoRemotes")}
								</p>
							) : (
								remotes.map((connection) => (
									<div
										key={connection.id}
										className={cn(
											`
											flex items-center justify-between gap-2
											rounded-md border border-border px-3 py-2
											`,
											editingId === connection.id &&
												"border-accent",
										)}
									>
										<div className="min-w-0">
											<p className="truncate text-sm font-medium text-foreground">
												{connection.label}
											</p>
											<p className="truncate text-xs text-muted">
												{connection.sshTarget}
											</p>
										</div>
										<div className="flex shrink-0 gap-1">
											<Button
												isIconOnly
												size="sm"
												variant="tertiary"
												aria-label={t("connEdit")}
												isDisabled={isBusy}
												onPress={() =>
													startEdit(connection)
												}
											>
												<PencilSquareIcon className="size-4" />
											</Button>
											<Button
												isIconOnly
												size="sm"
												variant="tertiary"
												aria-label={t("connRemove")}
												isDisabled={isBusy}
												onPress={() =>
													removeMutation.mutate(
														connection.id,
													)
												}
											>
												<TrashIcon className="size-4 text-danger" />
											</Button>
										</div>
									</div>
								))
							)}
						</div>

						<div className="flex flex-col gap-3 border-t border-border pt-4">
							<p className="text-sm font-medium text-foreground">
								{editingId !== null
									? t("connEditConnection")
									: t("connAddConnection")}
							</p>

							<TextField
								isRequired
								isInvalid={showErrors && errors.label != null}
								value={form.label}
								onChange={(value) => setField("label", value)}
							>
								<Label>{t("connFieldLabel")}</Label>
								<Input
									placeholder={t("connFieldLabelPlaceholder")}
								/>
								{showErrors && errors.label != null && (
									<FieldError>{t(errors.label)}</FieldError>
								)}
							</TextField>

							<div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end">
								<TextField
									isRequired
									isInvalid={
										showErrors && errors.sshTarget != null
									}
									value={form.sshTarget}
									onChange={(value) =>
										setField("sshTarget", value)
									}
								>
									<Label>{t("connFieldSshTarget")}</Label>
									<Input
										placeholder={t(
											"connFieldSshTargetPlaceholder",
										)}
									/>
									{showErrors && errors.sshTarget != null && (
										<FieldError>
											{t(errors.sshTarget)}
										</FieldError>
									)}
								</TextField>

								<Dropdown>
									<Button
										variant="secondary"
										className="w-full sm:w-auto"
										aria-label={t("connChooseSshAlias")}
										isDisabled={
											isBusy ||
											sshHostsLoading ||
											sshConfigHosts.length === 0
										}
									>
										<span className="truncate">
											{sshConfigHosts.length === 0
												? t("connNoSshAliases")
												: t("connChooseSshAlias")}
										</span>
										<ChevronDownIcon className="size-4 shrink-0 text-muted" />
									</Button>
									<Dropdown.Popover className="min-w-64 max-w-80">
										<Dropdown.Menu
											onAction={selectSshAlias}
										>
											<Dropdown.Section>
												<Header>
													{t("connSshAliases")}
												</Header>
												{sshConfigHosts.map((host) => (
													<Dropdown.Item
														key={host.alias}
														id={host.alias}
														textValue={host.alias}
													>
														<div className="min-w-0">
															<Label>
																{host.alias}
															</Label>
															{host.hostName !=
																null && (
																<Description className="truncate">
																	{
																		host.hostName
																	}
																</Description>
															)}
														</div>
													</Dropdown.Item>
												))}
											</Dropdown.Section>
										</Dropdown.Menu>
									</Dropdown.Popover>
								</Dropdown>
							</div>

							<TextField
								value={form.user}
								onChange={(value) => setField("user", value)}
							>
								<Label>{t("connFieldUser")}</Label>
								<Input
									placeholder={t("connFieldUserPlaceholder")}
								/>
							</TextField>

							<NumberField
								minValue={1}
								maxValue={65535}
								isInvalid={showErrors && errors.port != null}
								value={
									form.port === ""
										? Number.NaN
										: Number(form.port)
								}
								onChange={(value) =>
									setField(
										"port",
										Number.isNaN(value)
											? ""
											: String(value),
									)
								}
							>
								<Label>{t("connFieldPort")}</Label>
								<NumberField.Group>
									<NumberField.DecrementButton />
									<NumberField.Input />
									<NumberField.IncrementButton />
								</NumberField.Group>
								{showErrors && errors.port != null && (
									<FieldError>{t(errors.port)}</FieldError>
								)}
							</NumberField>

							<TextField
								value={form.remoteAghubPath}
								onChange={(value) =>
									setField("remoteAghubPath", value)
								}
							>
								<Label>{t("connFieldRemotePath")}</Label>
								<Input
									placeholder={t(
										"connFieldRemotePathPlaceholder",
									)}
								/>
								<Description>
									{t("connFieldRemotePathPlaceholder")}
								</Description>
							</TextField>

							{testResult != null && (
								<TestResultPanel result={testResult} t={t} />
							)}
						</div>
					</Modal.Body>

					<Modal.Footer className="flex-wrap">
						<Button
							variant="secondary"
							isDisabled={isBusy}
							onPress={handleTest}
						>
							{testMutation.isPending
								? t("connTesting")
								: t("connTestConnection")}
						</Button>
						<Button isDisabled={isBusy} onPress={handleSave}>
							{t("connSave")}
						</Button>
					</Modal.Footer>
				</Modal.Dialog>
			</Modal.Container>
		</Modal.Backdrop>
	);
}
