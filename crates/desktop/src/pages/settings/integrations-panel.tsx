import { ExclamationCircleIcon, TrashIcon } from "@heroicons/react/24/outline";
import {
	AlertDialog,
	Avatar,
	Button,
	Card,
	ListBox,
	Select,
	Spinner,
	Table,
	toast,
} from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import type { Key } from "react-aria-components";
import { useTranslation } from "react-i18next";
import type { CodeEditorType, CredentialResponse } from "../../generated/dto";
import { useApi } from "../../hooks/use-api";
import { useCurrentCodeEditor } from "../../hooks/use-integrations";
import {
	credentialsListQueryOptions,
	deleteCredentialMutationOptions,
} from "../../requests/credentials";
import {
	Empty,
	EmptyHeader,
	EmptyMedia,
	EmptyTitle,
} from "../../components/ui/empty";
import { CreateCredentialDialog } from "./components/create-credential-dialog";

const iconModules = import.meta.glob<{ default: string }>(
	"../../assets/agent/*.svg",
	{ eager: true, query: "?raw" },
);

const ICON_ALIASES: Record<string, string> = {
	vs_code_insiders: "vs_code",
	fleet: "rust_rover",
};

const UNDERSCORE_REGEX = /_/g;

function EditorIcon({ id, name }: { id: string; name: string }) {
	const resolvedId = ICON_ALIASES[id] ?? id;
	const path =
		iconModules[`../../assets/agent/${resolvedId}.svg`] ??
		iconModules[
			`../../assets/agent/${resolvedId.replace(UNDERSCORE_REGEX, "")}.svg`
		];
	const svg = path;

	if (svg) {
		return (
			<div
				className="flex size-5 shrink-0 items-center justify-center [&_svg]:size-4"
				// eslint-disable-next-line @eslint-react/dom-no-dangerously-set-innerhtml
				dangerouslySetInnerHTML={{
					__html: (svg.default || svg) as string,
				}}
			/>
		);
	}

	return (
		<Avatar size="sm" variant="soft" className="size-5">
			<Avatar.Fallback className="text-[10px]">
				{name.charAt(0).toUpperCase()}
			</Avatar.Fallback>
		</Avatar>
	);
}

export default function IntegrationsPanel() {
	const { t } = useTranslation();
	const { codeEditors, isLoading, selectedEditor, setCurrentEditor } =
		useCurrentCodeEditor();

	const api = useApi();
	const queryClient = useQueryClient();
	const [isCreateOpen, setIsCreateOpen] = useState(false);
	const [deleteTarget, setDeleteTarget] = useState<CredentialResponse | null>(
		null,
	);

	const {
		data: credentials = [],
		isLoading: isCredentialsLoading,
		isError: isCredentialsError,
		error: credentialsError,
		refetch: refetchCredentials,
	} = useQuery({
		...credentialsListQueryOptions({ api, enabled: true }),
	});

	const deleteMutation = useMutation({
		...deleteCredentialMutationOptions({
			api,
			queryClient,
			onSuccess: async () => {
				toast.success(t("credentialDeleted"));
				setDeleteTarget(null);
			},
		}),
		onError: (error) => {
			console.error("Failed to delete credential:", error);
			toast.danger(
				error instanceof Error
					? error.message
					: t("credentialDeleteFailed"),
			);
		},
	});

	const handleEditorChange = async (value: Key | null) => {
		if (!value) return;
		const editor = value as CodeEditorType;
		await setCurrentEditor(editor || undefined);
	};

	if (isLoading) {
		return (
			<div className="flex h-32 items-center justify-center">
				<Spinner size="lg" />
			</div>
		);
	}

	const installedEditors = codeEditors?.filter((e) => e.installed) || [];

	return (
		<div className="space-y-4">
			<Card className="p-4">
				<Card.Content className="space-y-4">
					<div className="flex items-center justify-between">
						<div className="space-y-0.5">
							<span className="text-sm font-medium text-(--foreground)">
								{t("codeEditors")}
							</span>
							<span className="block text-xs text-muted">
								{t("codeEditorsDescription")}
							</span>
						</div>
						<Select
							variant="secondary"
							selectedKey={selectedEditor || null}
							onSelectionChange={handleEditorChange}
							aria-label={t("codeEditors")}
							className="min-w-56"
						>
							<Select.Trigger>
								<Select.Value />
								<Select.Indicator />
							</Select.Trigger>
							<Select.Popover>
								<ListBox>
									{installedEditors.map((editor) => (
										<ListBox.Item
											key={editor.id}
											id={editor.id}
											textValue={editor.name}
										>
											<div className="flex items-center gap-2">
												<EditorIcon
													id={editor.id}
													name={editor.name}
												/>
												{editor.name}
											</div>
										</ListBox.Item>
									))}
								</ListBox>
							</Select.Popover>
						</Select>
					</div>
				</Card.Content>
			</Card>

			<Card className="p-0">
				<Card.Header className="flex flex-row items-start justify-between p-4">
					<div>
						<Card.Title>{t("credentials")}</Card.Title>
						<Card.Description>
							{t("credentialsDescription")}
						</Card.Description>
					</div>
					<Button onPress={() => setIsCreateOpen(true)}>
						{t("createCredential")}
					</Button>
				</Card.Header>
				<Card.Content className="space-y-4 p-4 pt-0">
					{isCredentialsError && (
						// Shown regardless of whether stale `credentials` rows are
						// still on screen: `Table.Body`'s `renderEmptyState` only
						// fires when `items` is empty, so a refetch failure with
						// cached data present would otherwise be invisible -- the
						// user would just see stale rows, no error, no retry.
						//
						// This is a persistent cached-data warning banner, not a
						// transient event -- an intentional, documented exception
						// to the toast-only rule (see ../../../AGENTS.md). It needs
						// its own accessible-name announcement instead of relying
						// on a toast that would have already disappeared by the
						// time a screen reader user tabs over here.
						<Empty
							role="alert"
							aria-live="polite"
							className="flex-row items-center justify-between gap-4 rounded-md border-solid border-danger/30 bg-danger/5 p-3 text-left md:p-3"
						>
							<EmptyHeader className="flex-row items-center gap-2 text-left">
								<EmptyMedia>
									<ExclamationCircleIcon className="size-5 shrink-0 text-danger" />
								</EmptyMedia>
								<EmptyTitle className="text-sm font-normal text-foreground">
									{credentialsError instanceof Error
										? credentialsError.message
										: t("unknownError")}
								</EmptyTitle>
							</EmptyHeader>
							<Button
								variant="secondary"
								size="sm"
								onPress={() => refetchCredentials()}
							>
								{t("retry")}
							</Button>
						</Empty>
					)}
					<Table>
						<Table.ScrollContainer>
							<Table.Content aria-label={t("credentials")}>
								<Table.Header>
									<Table.Column isRowHeader>
										{t("credentialName")}
									</Table.Column>
									<Table.Column>
										{t("credentialType")}
									</Table.Column>
									<Table.Column>{""}</Table.Column>
								</Table.Header>
								<Table.Body
									items={credentials}
									renderEmptyState={() => {
										// The error case is surfaced by the banner above
										// instead -- it must stay visible whether or not
										// `credentials` is empty, so it isn't handled here.
										if (
											isCredentialsLoading ||
											isCredentialsError
										) {
											return null;
										}
										return (
											<div className="py-8 text-center text-sm text-muted">
												{t("noCredentials")}
											</div>
										);
									}}
								>
									{(credential) => (
										<Table.Row id={credential.id}>
											<Table.Cell>
												{credential.name}
											</Table.Cell>
											<Table.Cell>
												{t("githubCredential")}
											</Table.Cell>
											<Table.Cell>
												<Button
													isIconOnly
													variant="tertiary"
													size="sm"
													onPress={() =>
														setDeleteTarget(
															credential,
														)
													}
												>
													<TrashIcon className="size-4" />
												</Button>
											</Table.Cell>
										</Table.Row>
									)}
								</Table.Body>
							</Table.Content>
						</Table.ScrollContainer>
					</Table>
				</Card.Content>
			</Card>

			<CreateCredentialDialog
				isOpen={isCreateOpen}
				onClose={() => setIsCreateOpen(false)}
				onSuccess={(_newId) => {
					toast.success(t("credentialCreated"));
				}}
			/>

			<AlertDialog.Backdrop
				isOpen={Boolean(deleteTarget)}
				onOpenChange={() => setDeleteTarget(null)}
			>
				<AlertDialog.Container>
					<AlertDialog.Dialog className="sm:max-w-[420px]">
						<AlertDialog.CloseTrigger />
						<AlertDialog.Header>
							<AlertDialog.Icon status="danger" />
							<AlertDialog.Heading>
								{t("deleteCredential")}
							</AlertDialog.Heading>
						</AlertDialog.Header>
						<AlertDialog.Body>
							{t("deleteCredentialConfirm")}
						</AlertDialog.Body>
						<AlertDialog.Footer>
							<Button
								variant="tertiary"
								onPress={() => setDeleteTarget(null)}
							>
								{t("cancel")}
							</Button>
							<Button
								variant="danger"
								isDisabled={deleteMutation.isPending}
								onPress={() => {
									if (deleteTarget)
										deleteMutation.mutate(deleteTarget.id);
								}}
							>
								{deleteMutation.isPending
									? t("deleting")
									: t("delete")}
							</Button>
						</AlertDialog.Footer>
					</AlertDialog.Dialog>
				</AlertDialog.Container>
			</AlertDialog.Backdrop>
		</div>
	);
}
