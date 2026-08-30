import { Button, SearchField } from "@heroui/react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLocation } from "wouter";
import featuredCatalog from "../../data/featured-skills.json";
import { useApi } from "../../hooks/use-api";
import {
	globalSkillLockQueryOptions,
	projectSkillLockQueryOptions,
} from "../../requests/skills";
import { FeaturedCard } from "./components/featured-card";
import { InstallModal } from "./components/install-modal";
import { SkillsHeader } from "./components/skills-header";
import { asMarketSkill, parseFeaturedCatalog } from "./featured";
import { useSkillInstall } from "./hooks/use-skill-install";
import { buildInstalledSet, isSkillInstalled } from "./installed-set";

// A malformed bundled catalog must not blank the whole page at import time.
// `featured.test.ts` is the real gate on the shipped file.
const FEATURED_SKILLS = (() => {
	try {
		return parseFeaturedCatalog(featuredCatalog);
	} catch {
		return [];
	}
})();

export default function SkillsShPage() {
	const { t } = useTranslation();
	const api = useApi();
	const [, setLocation] = useLocation();
	const [searchQuery, setSearchQuery] = useState("");
	const {
		installModalOpen,
		selectedSkill,
		selectedAgents,
		setSelectedAgents,
		installResults,
		isInstalling,
		skillAgents,
		installAll,
		setInstallAll,
		installToProject,
		setInstallToProject,
		canInstallToProject,
		selectedProjectId,
		setSelectedProjectId,
		projects,
		handleInstallClick,
		handleInstall,
		handleCloseInstallModal,
	} = useSkillInstall();

	const { data: globalLock } = useQuery(globalSkillLockQueryOptions({ api }));
	const projectLockResults = useQueries({
		queries: projects.map((project) =>
			projectSkillLockQueryOptions({ api, projectPath: project.path }),
		),
	});

	const installedSet = useMemo(() => {
		const entries = [
			...(globalLock?.skills ?? []),
			...projectLockResults.flatMap(
				(result) => result.data?.skills ?? [],
			),
		];
		return buildInstalledSet(entries);
	}, [globalLock, projectLockResults]);

	const handleSearch = () => {
		if (searchQuery.trim().length >= 2) {
			setLocation(
				`/skills-sh/search?q=${encodeURIComponent(searchQuery.trim())}`,
			);
		}
	};

	const handleKeyDown = (e: React.KeyboardEvent) => {
		if (e.key === "Enter") {
			handleSearch();
		}
	};

	return (
		<div className="flex h-full flex-col overflow-hidden p-6">
			<div className="flex shrink-0 flex-col items-center">
				<SkillsHeader
					size="large"
					searchQuery={searchQuery}
					onSearchQueryChange={setSearchQuery}
					onSearch={handleSearch}
					showSearchButton={false}
				/>
				<div className="mt-5 flex items-center gap-2">
					<SearchField
						value={searchQuery}
						onChange={setSearchQuery}
						onKeyDown={handleKeyDown}
						aria-label={t("searchMarketSkills")}
						className="w-[400px]"
					>
						<SearchField.Group>
							<SearchField.SearchIcon />
							<SearchField.Input
								placeholder={t("searchMarketSkillsPlaceholder")}
							/>
							<SearchField.ClearButton />
						</SearchField.Group>
					</SearchField>
					<Button
						onPress={handleSearch}
						isDisabled={searchQuery.trim().length < 2}
					>
						{t("search")}
					</Button>
				</div>
			</div>

			<div className="mt-8 min-h-0 flex-1 overflow-y-auto">
				<h2 className="mb-1 text-sm font-medium text-foreground">
					{t("featuredSkills")}
				</h2>
				<p className="mb-4 text-sm text-muted">
					{t("featuredSkillsHint")}
				</p>
				<div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
					{FEATURED_SKILLS.map((skill) => (
						<FeaturedCard
							key={`${skill.source}|${skill.name}`}
							skill={skill}
							installed={isSkillInstalled(
								installedSet,
								skill.source,
								skill.name,
							)}
							onInstall={() =>
								handleInstallClick(asMarketSkill(skill))
							}
						/>
					))}
				</div>
			</div>

			<InstallModal
				isOpen={installModalOpen}
				selectedSkill={selectedSkill}
				selectedAgents={selectedAgents}
				onSelectedAgentsChange={setSelectedAgents}
				installResults={installResults}
				isInstalling={isInstalling}
				skillAgents={skillAgents}
				installAll={installAll}
				onInstallAllChange={setInstallAll}
				installToProject={installToProject}
				canInstallToProject={canInstallToProject}
				onInstallToProjectChange={setInstallToProject}
				selectedProjectId={selectedProjectId}
				onSelectedProjectIdChange={setSelectedProjectId}
				projects={projects}
				onClose={handleCloseInstallModal}
				onInstall={handleInstall}
			/>
		</div>
	);
}
