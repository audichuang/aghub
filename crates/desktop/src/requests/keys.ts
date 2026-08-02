export const queryKeys = {
	plugins: {
		all: () => ["plugins"] as const,
		list: () => ["plugins", "list"] as const,
		detail: (pluginId: string) => ["plugins", "detail", pluginId] as const,
		detailDisabled: () => ["plugins", "detail", "__no_plugin__"] as const,
		market: () => ["plugins", "market"] as const,
		marketplaces: () => ["plugins", "marketplaces"] as const,
		cliStatus: () => ["plugins", "cli-status"] as const,
	},
	agents: {
		all: () => ["agents"] as const,
		list: () => ["agents", "list"] as const,
		availability: () => ["agents", "availability"] as const,
		coverage: (scope: string, projectRoot?: string | null) =>
			["agents", "coverage", scope, projectRoot ?? null] as const,
	},
	skills: {
		all: () => ["skills"] as const,
		lists: () => ["skills", "list"] as const,
		usage: () => ["skills", "usage"] as const,
		list: (
			scope: "global" | "project" | "all" = "global",
			projectRoot?: string,
		) => ["skills", "list", scope, projectRoot ?? null] as const,
		content: (
			path: string,
			scope: "global" | "project" = "global",
			projectRoot?: string,
		) => ["skills", "content", path, scope, projectRoot ?? null] as const,
		tree: (
			path: string,
			scope: "global" | "project" = "global",
			projectRoot?: string,
		) => ["skills", "tree", path, scope, projectRoot ?? null] as const,
		updateChecks: (
			scope: "global" | "project" | "all" = "global",
			projectRoot?: string,
		) => ["skills", "check-updates", scope, projectRoot ?? null] as const,
		/// Prefix covering every scope's update check — an applied update
		/// invalidates the outdated-or-not answer for all of them.
		updateChecksAll: () => ["skills", "check-updates"] as const,
		/// Prefixes for a skill's rendered BODY — the SKILL.md text and its file
		/// tree. A flow that rewrites files on disk owes these a refetch: the
		/// path (and so the key) is unchanged, so nothing else brings the open
		/// detail panel up to date.
		contents: () => ["skills", "content"] as const,
		trees: () => ["skills", "tree"] as const,
		lock: {
			all: () => ["skills", "lock"] as const,
			global: () => ["skills", "lock", "global"] as const,
			project: (projectPath?: string) =>
				["skills", "lock", "project", projectPath ?? null] as const,
		},
		pruneLock: (
			scope: "global" | "project" | "all" = "global",
			projectRoot?: string | null,
		) => ["skills", "prune-lock", scope, projectRoot ?? null] as const,
		sources: {
			all: () => ["skills", "sources"] as const,
			list: (
				scope: "global" | "project" | "all" = "global",
				projectRoot?: string,
			) =>
				[
					"skills",
					"sources",
					"list",
					scope,
					projectRoot ?? null,
				] as const,
			diff: (
				source: string,
				scope: "global" | "project" | "all" = "global",
				projectRoot?: string,
				gitRef?: string,
			) =>
				[
					"skills",
					"sources",
					"diff",
					source,
					scope,
					projectRoot ?? null,
					gitRef ?? null,
				] as const,
		},
	},
	mcps: {
		all: () => ["mcps"] as const,
		lists: () => ["mcps", "list"] as const,
		list: (
			scope: "global" | "project" | "all" = "global",
			projectRoot?: string,
		) => ["mcps", "list", scope, projectRoot ?? null] as const,
		detail: (
			name: string,
			agent: string,
			scope: "global" | "project" | "all",
		) => ["mcps", "detail", name, agent, scope] as const,
	},
	subAgents: {
		all: () => ["sub-agents"] as const,
		list: (
			scope: "global" | "project" | "all" = "global",
			projectRoot?: string,
		) => ["sub-agents", "list", scope, projectRoot ?? null] as const,
		detail: (
			name: string,
			agent: string,
			scope: "global" | "project" | "all",
		) => ["sub-agents", "detail", name, agent, scope] as const,
	},
	/// "Does the machine running aghub-api already have a git credential for
	/// this URL?" — an answer about the HOST, so the connection is part of the
	/// key: without it, switching to another VM re-uses the previous machine's
	/// answer for the same URL. Its OWN namespace on purpose — parked under
	/// `skills` it would be swept stale by every skill mutation, and each
	/// re-probe spawns a `git credential fill` on the target machine.
	gitCredentialStatus: {
		all: () => ["git-credential-status"] as const,
		of: (url: string, connectionId: string) =>
			["git-credential-status", connectionId, url] as const,
	},
	credentials: {
		all: () => ["credentials"] as const,
		list: () => ["credentials", "list"] as const,
		sourceBindings: () => ["credentials", "source-bindings"] as const,
	},
	inferenceProviders: {
		all: () => ["inference-providers"] as const,
		list: () => ["inference-providers", "list"] as const,
		presets: () => ["inference-providers", "presets"] as const,
		agent: (agentId: string) =>
			["inference-providers", "agent", agentId] as const,
		agentState: (agentId: string) =>
			["inference-providers", "agent", agentId, "state"] as const,
		password: (name: string) =>
			["inference-providers", "password", name] as const,
	},
	integrations: {
		all: () => ["integrations"] as const,
		codeEditors: () => ["integrations", "code-editors"] as const,
	},
	market: {
		all: () => ["market"] as const,
		search: (query: string) => ["market", "search", query] as const,
	},
};
