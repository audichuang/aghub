import { Button, Label, ListBox, Modal, Select, toast } from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import {
	bindSourceCredentialMutationOptions,
	credentialsListQueryOptions,
	sourceCredentialBindingsQueryOptions,
} from "../requests/credentials";
import { CreateCredentialDialog } from "../pages/settings/components/create-credential-dialog";

interface SourceCredentialBindingDialogProps {
	isOpen: boolean;
	bindingSource: string;
	defaultCredentialHost?: string;
	onClose: () => void;
	onBound: () => void;
}

function hostFromSource(source: string): string {
	try {
		return new URL(source).host;
	} catch {
		return "";
	}
}

export function SourceCredentialBindingDialog({
	isOpen,
	bindingSource,
	defaultCredentialHost,
	onClose,
	onBound,
}: SourceCredentialBindingDialogProps) {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const [selectedCredentialId, setSelectedCredentialId] = useState<
		string | null
	>(null);
	const [createDialogOpen, setCreateDialogOpen] = useState(false);

	const credentialHost = useMemo(
		() => defaultCredentialHost || hostFromSource(bindingSource),
		[defaultCredentialHost, bindingSource],
	);

	const { data: credentials = [] } = useQuery({
		...credentialsListQueryOptions({ api, enabled: isOpen }),
	});
	const { data: sourceBindings = [] } = useQuery({
		...sourceCredentialBindingsQueryOptions({ api, enabled: isOpen }),
	});

	const defaultCredentialId = useMemo(() => {
		if (!isOpen) {
			return "";
		}

		const existingBinding = sourceBindings.find(
			(binding) => binding.source === bindingSource,
		);
		if (existingBinding?.credentialId) {
			return existingBinding.credentialId;
		}

		if (credentialHost) {
			const hostCredential = credentials.find(
				(credential) => credential.name === credentialHost,
			);
			if (hostCredential) {
				return hostCredential.id;
			}
		}

		return "";
	}, [bindingSource, credentialHost, credentials, isOpen, sourceBindings]);
	const activeCredentialId = selectedCredentialId ?? defaultCredentialId;

	const bindMutation = useMutation(
		bindSourceCredentialMutationOptions({
			api,
			queryClient,
			onSuccess: async () => {
				toast.success(t("sourceCredentialBound"));
				setSelectedCredentialId(null);
				setCreateDialogOpen(false);
				onBound();
				onClose();
			},
			onError: () => toast.danger(t("sourceCredentialBindingError")),
		}),
	);

	const bindCredential = (credentialId: string) => {
		bindMutation.mutate({
			source: bindingSource,
			credentialId,
		});
	};

	const handleClose = () => {
		if (bindMutation.isPending) {
			return;
		}
		setCreateDialogOpen(false);
		setSelectedCredentialId(null);
		onClose();
	};

	return (
		<>
			<Modal.Backdrop
				isOpen={isOpen}
				onOpenChange={(open) => {
					if (!open) handleClose();
				}}
			>
				<Modal.Container>
					<Modal.Dialog className="max-w-md">
						<Modal.CloseTrigger />
						<Modal.Header>
							<Modal.Heading>
								{t("bindCredentialToSource")}
							</Modal.Heading>
						</Modal.Header>
						<Modal.Body className="space-y-4 p-2">
							<div className="space-y-1">
								<Label>{t("source")}</Label>
								<code className="block max-w-full overflow-hidden rounded-md bg-surface-secondary px-2 py-1.5 font-mono text-xs text-muted text-ellipsis whitespace-nowrap">
									{bindingSource}
								</code>
							</div>

							{credentials.length > 0 ? (
								<Select
									className="w-full"
									variant="secondary"
									selectedKey={
										activeCredentialId || undefined
									}
									isDisabled={bindMutation.isPending}
									onSelectionChange={(key) => {
										if (!key) return;
										setSelectedCredentialId(String(key));
									}}
								>
									<Label>{t("selectCredential")}</Label>
									<Select.Trigger>
										<Select.Value />
										<Select.Indicator />
									</Select.Trigger>
									<Select.Popover>
										<ListBox>
											{credentials.map((credential) => (
												<ListBox.Item
													key={credential.id}
													id={credential.id}
													textValue={credential.name}
												>
													{credential.name}
													<ListBox.ItemIndicator />
												</ListBox.Item>
											))}
										</ListBox>
									</Select.Popover>
								</Select>
							) : (
								<div className="rounded-md border border-border bg-surface-secondary px-3 py-2 text-sm text-muted">
									{t("noCredentialsAvailable")}
								</div>
							)}
						</Modal.Body>
						<Modal.Footer>
							<Button
								type="button"
								variant="secondary"
								onPress={handleClose}
							>
								{t("cancel")}
							</Button>
							<Button
								type="button"
								variant="secondary"
								isDisabled={bindMutation.isPending}
								onPress={() => setCreateDialogOpen(true)}
							>
								{t("createCredentialAndBind")}
							</Button>
							<Button
								type="button"
								isDisabled={
									!activeCredentialId ||
									bindMutation.isPending
								}
								onPress={() =>
									bindCredential(activeCredentialId)
								}
							>
								{t("bind")}
							</Button>
						</Modal.Footer>
					</Modal.Dialog>
				</Modal.Container>
			</Modal.Backdrop>

			<CreateCredentialDialog
				key={credentialHost}
				isOpen={createDialogOpen}
				defaultName={credentialHost}
				onClose={() => setCreateDialogOpen(false)}
				onSuccess={(credentialId) => {
					setCreateDialogOpen(false);
					setSelectedCredentialId(credentialId);
					bindCredential(credentialId);
				}}
			/>
		</>
	);
}
