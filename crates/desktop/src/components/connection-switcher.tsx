import { ChevronUpDownIcon, Cog6ToothIcon } from "@heroicons/react/24/solid";
import {
	Button,
	Dropdown,
	Header,
	Label,
	type Selection,
	Separator,
} from "@heroui/react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useConnection } from "../hooks/use-connection";
import {
	LOCAL_CONNECTION,
	type ConnectionStatus,
} from "../lib/connection-logic";
import { cn } from "../lib/utils";
import { ManageConnectionsDialog } from "./manage-connections-dialog";

/** Sentinel key for the non-selectable "Manage connections…" action row. */
const MANAGE_ACTION = "__manage__";

/** Tailwind classes for the status dot, keyed by the 4-state status. */
const STATUS_DOT: Record<ConnectionStatus, string> = {
	idle: "bg-muted",
	connecting: "bg-warning animate-pulse",
	connected: "bg-success",
	error: "bg-danger",
};

function StatusDot({ status }: { status: ConnectionStatus }) {
	return (
		<span
			className={cn("size-2 shrink-0 rounded-full", STATUS_DOT[status])}
			aria-hidden="true"
		/>
	);
}

export function ConnectionSwitcher() {
	const { t } = useTranslation();
	const { connections, activeId, activeConnection, status, setActive } =
		useConnection();
	const [isManageOpen, setIsManageOpen] = useState(false);

	// The implicit Local connection carries a hardcoded label; render it via the
	// localized key instead so it follows the UI language.
	const labelOf = (c: { id: string; label: string }) =>
		c.id === LOCAL_CONNECTION.id ? t("connLocal") : c.label;

	// react-aria selection is a Set<Key>; the switcher is single-select so we
	// read at most one key out of the change and ignore "all"/empty.
	const handleSelectionChange = (keys: Selection) => {
		if (keys === "all") return;
		const next = [...keys][0];
		if (typeof next === "string" && next !== activeId) {
			setActive(next);
		}
	};

	return (
		<>
			<Dropdown>
				<Button
					variant="secondary"
					aria-label={t("connSwitcherLabel")}
					className="w-full justify-between"
				>
					<span className="flex min-w-0 items-center gap-2">
						<StatusDot status={status} />
						<span className="truncate">
							{labelOf(activeConnection)}
						</span>
					</span>
					<ChevronUpDownIcon className="size-4 shrink-0 text-muted" />
				</Button>
				<Dropdown.Popover className="min-w-[208px]">
					<Dropdown.Menu
						onAction={(key) => {
							if (key === MANAGE_ACTION) {
								setIsManageOpen(true);
							}
						}}
					>
						<Dropdown.Section
							selectionMode="single"
							selectedKeys={new Set([activeId])}
							onSelectionChange={handleSelectionChange}
						>
							<Header>{t("connSelectConnection")}</Header>
							{connections.map((connection) => (
								<Dropdown.Item
									key={connection.id}
									id={connection.id}
									textValue={labelOf(connection)}
								>
									<Dropdown.ItemIndicator />
									{/* The dot reflects bring-up status only for
									    the active connection; others are idle. */}
									<StatusDot
										status={
											connection.id === activeId
												? status
												: "idle"
										}
									/>
									<Label>{labelOf(connection)}</Label>
								</Dropdown.Item>
							))}
						</Dropdown.Section>
						<Separator />
						<Dropdown.Item
							id={MANAGE_ACTION}
							textValue={t("connManageConnections")}
						>
							<Cog6ToothIcon className="size-4 text-muted" />
							<Label>{t("connManageConnections")}</Label>
						</Dropdown.Item>
					</Dropdown.Menu>
				</Dropdown.Popover>
			</Dropdown>
			<ManageConnectionsDialog
				isOpen={isManageOpen}
				onClose={() => setIsManageOpen(false)}
			/>
		</>
	);
}
