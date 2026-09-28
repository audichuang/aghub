import { UserGroupIcon } from "@heroicons/react/24/solid";
import {
	Card,
	SearchField,
	ToggleButton,
	ToggleButtonGroup,
} from "@heroui/react";
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { AgentCard } from "../../components/agent-card";
import { useAgentAvailability } from "../../hooks/use-agent-availability";
import { useApi } from "../../hooks/use-api";
import { useConnection } from "../../hooks/use-connection";
import { setAgentDisabled } from "../../lib/store";
import { queryKeys } from "../../requests/keys";

export default function AgentsPanel() {
	const { t } = useTranslation();
	const { availableAgents, refreshDisabledAgents } = useAgentAvailability();
	const api = useApi();
	const queryClient = useQueryClient();
	// Toggling while connected to a remote writes THAT server's selection.
	const { activeId } = useConnection();
	const [updating, setUpdating] = useState<string | null>(null);
	const [agentFilter, setAgentFilter] = useState<
		"all" | "enabled" | "disabled"
	>("all");
	const [agentSearch, setAgentSearch] = useState("");

	const handleToggleAgent = async (
		agentId: string,
		currentlyDisabled: boolean,
	) => {
		setUpdating(agentId);
		try {
			await setAgentDisabled(api, activeId, agentId, !currentlyDisabled);
			await refreshDisabledAgents();
			// The server plans repair around this selection, so the migration
			// banner's preview is stale the moment it changes.
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.repairPreviews(),
			});
		} finally {
			setUpdating(null);
		}
	};

	let filteredAgents = availableAgents.filter(
		(agent) => agent.availability.is_available,
	);

	if (agentSearch.trim()) {
		const search = agentSearch.toLowerCase();
		filteredAgents = filteredAgents.filter(
			(agent) =>
				agent.display_name.toLowerCase().includes(search) ||
				agent.id.toLowerCase().includes(search),
		);
	}

	if (agentFilter === "enabled") {
		filteredAgents = filteredAgents.filter((agent) => !agent.isDisabled);
	} else if (agentFilter === "disabled") {
		filteredAgents = filteredAgents.filter((agent) => agent.isDisabled);
	}

	// Sort: enabled agents first, disabled agents last
	filteredAgents.sort((a, b) => {
		if (a.isDisabled === b.isDisabled) return 0;
		return a.isDisabled ? 1 : -1;
	});

	return (
		<div className="space-y-3">
			{/* Search and Filter Bar */}
			<div
				className="
      flex flex-col gap-2
      sm:flex-row sm:items-center sm:justify-between
    "
			>
				<SearchField
					value={agentSearch}
					onChange={setAgentSearch}
					className="
       w-full
       sm:w-64
     "
				>
					<SearchField.Group>
						<SearchField.SearchIcon />
						<SearchField.Input placeholder={t("searchAgents")} />
						<SearchField.ClearButton />
					</SearchField.Group>
				</SearchField>
				<ToggleButtonGroup
					selectedKeys={[agentFilter]}
					onSelectionChange={(keys) =>
						setAgentFilter(
							[...keys][0] as "all" | "enabled" | "disabled",
						)
					}
					selectionMode="single"
					disallowEmptySelection
					size="sm"
				>
					<ToggleButton
						id="all"
						variant="ghost"
						className={"bg-surface"}
					>
						{t("all")}
					</ToggleButton>
					<ToggleButtonGroup.Separator />
					<ToggleButton
						id="enabled"
						variant="ghost"
						className={"bg-surface"}
					>
						{t("enabled")}
					</ToggleButton>
					<ToggleButtonGroup.Separator />
					<ToggleButton
						id="disabled"
						variant="ghost"
						className={"bg-surface"}
					>
						{t("disabled")}
					</ToggleButton>
				</ToggleButtonGroup>
			</div>

			{/* Agents Card */}
			<Card className="bg-surface" variant="transparent">
				<Card.Content>
					{filteredAgents.length === 0 ? (
						<div
							className="
              flex flex-col items-center justify-center py-16
              text-center
            "
						>
							<div className="mb-4 text-muted">
								<UserGroupIcon className="mx-auto size-12" />
							</div>
							<p className="text-sm font-medium text-(--foreground)">
								{agentSearch || agentFilter !== "all"
									? t("noAgentsMatch")
									: t("noAgentsAvailable")}
							</p>
							<p className="mt-1 max-w-sm text-xs text-muted">
								{agentSearch || agentFilter !== "all"
									? t("adjustFiltersDescription")
									: t("noAgentsDescription")}
							</p>
						</div>
					) : (
						<div
							className="
              grid grid-cols-1 gap-3
              sm:grid-cols-2
            "
						>
							{filteredAgents.map((agent) => (
								<AgentCard
									key={agent.id}
									agent={agent}
									isUpdating={updating === agent.id}
									onToggle={handleToggleAgent}
								/>
							))}
						</div>
					)}
				</Card.Content>
			</Card>
		</div>
	);
}
