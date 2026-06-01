import { Cog6ToothIcon } from "@heroicons/react/24/solid";
import { Surface } from "@heroui/react";
import { useTranslation } from "react-i18next";
import { Link, useLocation } from "wouter";
import { useSidebarNavigation } from "../hooks/use-sidebar-navigation";
import { isSidebarHrefActive } from "../lib/sidebar-navigation";
import { cn } from "../lib/utils";
import { ConnectionSwitcher } from "./connection-switcher";
import { ProjectList } from "./project-list";

export function AppSidebar() {
	const { t } = useTranslation();
	const [pathname] = useLocation();
	const { visibleSidebarItems } = useSidebarNavigation();

	return (
		<Surface
			variant="secondary"
			data-tour="sidebar"
			className="flex w-60 shrink-0 flex-col border-r border-border p-3"
		>
			<aside className="flex h-full flex-col">
				<div className="mb-2">
					<ConnectionSwitcher />
				</div>
				<nav className="flex flex-col gap-0.5">
					{visibleSidebarItems.map((item) => {
						const Icon = item.icon;
						const isActive = isSidebarHrefActive(
							pathname,
							item.href,
						);

						return (
							<Link
								key={item.id}
								href={item.href}
								data-tour={item.tour}
								className={cn(
									`
           flex items-center gap-2.5 rounded-md px-2.5 py-1.5 text-sm
           transition-colors
         `,
									isActive
										? "bg-surface font-medium text-foreground"
										: `
            text-muted
            hover:bg-surface-secondary hover:text-foreground
          `,
								)}
							>
								<Icon className="size-4" />
								<span>{t(item.labelKey)}</span>
							</Link>
						);
					})}
				</nav>
				<div data-tour="project-section">
					<ProjectList />
				</div>

				<nav className="mt-auto">
					<Link
						href="/settings"
						data-tour="nav-settings"
						className={cn(
							`
         flex items-center gap-2.5 rounded-md px-2.5 py-1.5 text-sm
         transition-colors
       `,
							isSidebarHrefActive(pathname, "/settings")
								? "bg-surface font-medium text-foreground"
								: `
          text-muted
          hover:bg-surface-secondary hover:text-foreground
        `,
						)}
					>
						<Cog6ToothIcon className="size-4" />
						<span>{t("settings")}</span>
					</Link>
				</nav>
			</aside>
		</Surface>
	);
}
