import { Spinner, Toast, toast } from "@heroui/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import {
	getCurrent as getCurrentDeepLinks,
	onOpenUrl,
} from "@tauri-apps/plugin-deep-link";
import { NuqsAdapter } from "nuqs/adapters/react";
import { Suspense, useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useKeyBindings } from "rooks";
import { Route, Router, Switch, useLocation } from "wouter";
import { ConnectionGate } from "./components/connection-gate";
import { DeepLinkImportModal } from "./components/deep-link-import-modal";
import { OnboardingController } from "./components/onboarding-controller";
import { Redirect } from "./components/redirect";
import { ErrorBoundary } from "./components/ui/error-boundary";
import { useSidebarNavigation } from "./hooks/use-sidebar-navigation";
import { MainLayout } from "./layouts/main-layout";
import type { DeepLinkImportIntent } from "./lib/deep-link";
import { parseDeepLink } from "./lib/deep-link";
import { setupAppMenu } from "./lib/menu";
import { initStore } from "./lib/store";
import InferenceProvidersPage from "./pages/inference-providers";
import PluginsPage from "./pages/plugins";
import ProjectDetailPage from "./pages/project/detail";
import SettingsPage from "./pages/settings";
import CoveragePage from "./pages/settings/coverage";
import CustomAgentsPage from "./pages/settings/custom-agents";
import MCPServersPage from "./pages/settings/mcp-servers";
import SkillsPage from "./pages/settings/skills";
import SubAgentsPage from "./pages/settings/sub-agents";
import SkillsShPage from "./pages/skills-sh";
import SkillsSearchPage from "./pages/skills-sh/search";
import { ConnectionProvider } from "./providers/connection";
import { ThemeProvider } from "./providers/theme";
import "./lib/i18n";

const queryClient = new QueryClient({
	defaultOptions: {
		queries: {
			retry: 1,
			refetchOnWindowFocus: false,
		},
	},
});

function SkillsPageSkeleton() {
	return (
		<div className="flex h-full">
			<div
				className="
      flex w-80 shrink-0 items-center justify-center border-r border-border
    "
			>
				<Spinner />
			</div>
			<div className="flex-1" />
		</div>
	);
}

function DefaultSidebarRoute() {
	const { defaultHref, isLoading } = useSidebarNavigation();

	if (isLoading) {
		return null;
	}

	return <Redirect to={defaultHref} />;
}

function SourcesRedirect() {
	const params = new URLSearchParams(window.location.search);
	const source = params.get("source");
	const to = source
		? `/skills?view=source&source=${encodeURIComponent(source)}`
		: "/skills?view=source";
	return <Redirect to={to} />;
}

function App() {
	const [isStoreReady, setIsStoreReady] = useState(false);
	const [pendingIntents, setPendingIntents] = useState<
		DeepLinkImportIntent[]
	>([]);
	const [, setLocation] = useLocation();
	const { t, i18n } = useTranslation();

	const currentIntent = pendingIntents[0] ?? null;

	const processNextIntent = useCallback(() => {
		setPendingIntents((prev) => prev.slice(1));
	}, []);

	useEffect(() => {
		setupAppMenu(t);
	}, [t, i18n.language]);

	useEffect(() => {
		initStore()
			.then(() => setIsStoreReady(true))
			.catch((err) => {
				console.error("Failed to initialize store:", err);
			});
	}, []);

	useEffect(() => {
		const unlisten = listen<string>("navigate", (event) => {
			setLocation(event.payload);
		});
		return () => {
			unlisten.then((fn) => fn());
		};
	}, [setLocation]);

	useEffect(() => {
		let isMounted = true;
		let unlistenDeepLink: (() => void) | null = null;

		const handleUrls = (urls: string[] | null) => {
			if (!isMounted || !urls || urls.length === 0) {
				return;
			}

			const newIntents = urls
				.map(parseDeepLink)
				.filter((result) => {
					if (!result.ok) {
						toast.danger(t(result.error));
					}
					return result.ok;
				})
				.map((result) => result.intent);

			if (newIntents.length > 0) {
				setPendingIntents((prev) => prev.concat(newIntents));
			}
		};

		void getCurrentDeepLinks()
			.then(handleUrls)
			.catch((error) => {
				console.error("Failed to read current deep link:", error);
			});

		void onOpenUrl((urls) => {
			handleUrls(urls);
		})
			.then((dispose) => {
				unlistenDeepLink = dispose;
			})
			.catch((error) => {
				console.error("Failed to subscribe to deep links:", error);
			});

		return () => {
			isMounted = false;
			unlistenDeepLink?.();
		};
	}, [t]);

	useKeyBindings({
		",": (event) => {
			if (event.metaKey && !event.ctrlKey && !event.altKey) {
				event.preventDefault();
				setLocation("/settings");
			}
		},
	});

	if (!isStoreReady) {
		return (
			<div className="flex h-screen items-center justify-center">
				<Spinner size="lg" />
			</div>
		);
	}

	return (
		<QueryClientProvider client={queryClient}>
			<Toast.Provider placement="bottom end" />
			<ThemeProvider>
				<ConnectionProvider>
					<NuqsAdapter>
						<Router>
							<OnboardingController />
							<MainLayout>
								<ConnectionGate>
									<Switch>
										<Route path="/">
											<DefaultSidebarRoute />
										</Route>
										<Route path="/skills">
											<ErrorBoundary>
												<Suspense
													fallback={
														<SkillsPageSkeleton />
													}
												>
													<SkillsPage />
												</Suspense>
											</ErrorBoundary>
										</Route>
										<Route path="/coverage">
											<ErrorBoundary>
												<CoveragePage />
											</ErrorBoundary>
										</Route>
										<Route path="/mcp">
											<ErrorBoundary>
												<Suspense
													fallback={
														<SkillsPageSkeleton />
													}
												>
													<MCPServersPage />
												</Suspense>
											</ErrorBoundary>
										</Route>
										<Route path="/inference-providers">
											<ErrorBoundary>
												<InferenceProvidersPage />
											</ErrorBoundary>
										</Route>
										<Route path="/skills-sh/search">
											<ErrorBoundary>
												<Suspense
													fallback={
														<SkillsPageSkeleton />
													}
												>
													<SkillsSearchPage />
												</Suspense>
											</ErrorBoundary>
										</Route>
										<Route path="/skills-sh">
											<ErrorBoundary>
												<Suspense
													fallback={
														<SkillsPageSkeleton />
													}
												>
													<SkillsShPage />
												</Suspense>
											</ErrorBoundary>
										</Route>
										<Route path="/cc-plugins">
											<ErrorBoundary>
												<Suspense
													fallback={
														<SkillsPageSkeleton />
													}
												>
													<PluginsPage />
												</Suspense>
											</ErrorBoundary>
										</Route>
										<Route path="/settings">
											<SettingsPage />
										</Route>
										<Route path="/settings/custom-agents">
											<CustomAgentsPage />
										</Route>
										<Route path="/sub-agents">
											<ErrorBoundary>
												<Suspense
													fallback={
														<SkillsPageSkeleton />
													}
												>
													<SubAgentsPage />
												</Suspense>
											</ErrorBoundary>
										</Route>
										<Route path="/projects/:id">
											<ProjectDetailPage />
										</Route>
										<Route path="/sources">
											<SourcesRedirect />
										</Route>
										<Route>
											<DefaultSidebarRoute />
										</Route>
									</Switch>
									<DeepLinkImportModal
										intent={currentIntent}
										onComplete={processNextIntent}
									/>
								</ConnectionGate>
							</MainLayout>
						</Router>
					</NuqsAdapter>
				</ConnectionProvider>
			</ThemeProvider>
		</QueryClientProvider>
	);
}

export default App;
