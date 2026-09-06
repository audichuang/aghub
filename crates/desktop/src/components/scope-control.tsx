import { ListBox, Select } from "@heroui/react";
import { useTranslation } from "react-i18next";
import { useProjects } from "../hooks/use-projects";
import { cn } from "../lib/utils";

// Internal to this component's own Select item ids — not a general-purpose
// URL encoding. Callers that need to parse a "project:<path>" URL param use
// `lib/skills-page-url.ts`'s own copy of this prefix instead of importing it
// from here (a pure `lib/*.ts` module cannot import a `.tsx` file).
const PROJECT_KEY_PREFIX = "project:";

interface ScopeControlProps {
	scope: "global" | "project";
	selectedProjectPath: string | null;
	onChange: (scope: "global" | "project", projectPath: string | null) => void;
	/**
	 * Override the trigger's width constraint. Defaults to `max-w-[48%]`,
	 * which `pages/settings/coverage.tsx` relies on and must keep getting
	 * when it does not pass this prop.
	 */
	className?: string;
}

/**
 * Single dropdown that selects the active scope: "Global" or one of the open
 * projects. Replaces the old segmented switch + separate project Select — one
 * control, one choice, far less header chrome in the narrow list column.
 * Shared by the Skills page and the Coverage page.
 */
export function ScopeControl({
	scope,
	selectedProjectPath,
	onChange,
	className,
}: ScopeControlProps) {
	const { t } = useTranslation();
	const { data: projects = [] } = useProjects();

	// Map (scope, projectPath) → a single Select key. A project scope with no
	// resolved path (e.g. restored from a stale URL) falls back to no selection
	// so the trigger shows the placeholder rather than a phantom value.
	const selectedKey =
		scope === "global"
			? "global"
			: selectedProjectPath
				? `${PROJECT_KEY_PREFIX}${selectedProjectPath}`
				: undefined;

	return (
		<Select
			aria-label={t("scope")}
			className={cn("min-w-0 max-w-[48%]", className)}
			variant="secondary"
			selectedKey={selectedKey}
			placeholder={t("scopeSwitchProject")}
			onSelectionChange={(key) => {
				const k = String(key);
				if (k === "global") {
					onChange("global", null);
				} else if (k.startsWith(PROJECT_KEY_PREFIX)) {
					onChange("project", k.slice(PROJECT_KEY_PREFIX.length));
				}
			}}
		>
			<Select.Trigger>
				<Select.Value />
				<Select.Indicator />
			</Select.Trigger>
			<Select.Popover>
				<ListBox>
					<ListBox.Item
						id="global"
						textValue={t("scopeSwitchGlobal")}
					>
						{t("scopeSwitchGlobal")}
						<ListBox.ItemIndicator />
					</ListBox.Item>
					{projects.map((p) => (
						<ListBox.Item
							key={p.path}
							id={`${PROJECT_KEY_PREFIX}${p.path}`}
							textValue={p.name}
						>
							{p.name}
							<ListBox.ItemIndicator />
						</ListBox.Item>
					))}
				</ListBox>
			</Select.Popover>
		</Select>
	);
}
