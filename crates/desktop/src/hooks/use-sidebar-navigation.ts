import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo } from "react";
import {
	DEFAULT_SIDEBAR_ITEMS,
	getSidebarItems,
	saveSidebarItems,
	type SidebarItemId,
	type SidebarItemPreference,
} from "../lib/store";
import {
	PreferenceNotReadError,
	preferenceWriteBasis,
	writePreference,
} from "../lib/preference-write";
import {
	getDefaultSidebarHref,
	normalizeSidebarItems,
	resolveSidebarItems,
} from "../lib/sidebar-navigation";

const SIDEBAR_NAVIGATION_QUERY_KEY = ["sidebar-navigation"];

export function useSidebarNavigation() {
	const queryClient = useQueryClient();
	const { data, isLoading, isSuccess, isError, refetch } = useQuery({
		queryKey: SIDEBAR_NAVIGATION_QUERY_KEY,
		queryFn: getSidebarItems,
	});

	// A RENDERING fallback only. The app still needs a sidebar to draw when the
	// preference read fails, but nothing below may treat this as the user's
	// saved state — see `preferenceWriteBasis`.
	const sidebarItems = useMemo(
		() => normalizeSidebarItems(data ?? DEFAULT_SIDEBAR_ITEMS),
		[data],
	);
	const resolvedSidebarItems = useMemo(
		() => resolveSidebarItems(sidebarItems),
		[sidebarItems],
	);
	const visibleSidebarItems = useMemo(
		() => resolvedSidebarItems.filter((item) => item.visible),
		[resolvedSidebarItems],
	);
	const defaultHref = useMemo(
		() => getDefaultSidebarHref(sidebarItems),
		[sidebarItems],
	);

	const updateSidebarItems = useCallback(
		async (
			updater: (
				current: SidebarItemPreference[],
			) => SidebarItemPreference[],
		) => {
			// `sidebarItems` used to be the fallback here, which is how a
			// failed read got persisted: it resolves to DEFAULT_SIDEBAR_ITEMS,
			// so one checkbox click wrote the defaults over a list the user had
			// hidden and reordered. There is no basis until the read succeeds.
			const basis = preferenceWriteBasis(
				isSuccess,
				queryClient.getQueryData(SIDEBAR_NAVIGATION_QUERY_KEY) as
					| SidebarItemPreference[]
					| undefined,
				DEFAULT_SIDEBAR_ITEMS,
			);
			if (basis === null) {
				throw new PreferenceNotReadError("sidebar");
			}
			const previous = normalizeSidebarItems(basis);
			const next = normalizeSidebarItems(updater(previous));

			await writePreference({
				previous,
				next,
				setCache: (value) =>
					queryClient.setQueryData(
						SIDEBAR_NAVIGATION_QUERY_KEY,
						value,
					),
				save: saveSidebarItems,
			});
		},
		[queryClient, isSuccess],
	);

	const setItemVisibility = useCallback(
		async (id: SidebarItemId, visible: boolean) => {
			await updateSidebarItems((current) => {
				const visibleCount = current.filter(
					(item) => item.visible,
				).length;
				const isLastVisibleItem =
					!visible &&
					visibleCount === 1 &&
					current.some((item) => item.id === id && item.visible);

				if (isLastVisibleItem) {
					return current;
				}

				return current.map((item) =>
					item.id === id ? { ...item, visible } : item,
				);
			});
		},
		[updateSidebarItems],
	);

	const moveItem = useCallback(
		async (id: SidebarItemId, direction: "up" | "down") => {
			await updateSidebarItems((current) => {
				const index = current.findIndex((item) => item.id === id);
				const targetIndex = direction === "up" ? index - 1 : index + 1;

				if (
					index === -1 ||
					targetIndex < 0 ||
					targetIndex >= current.length
				) {
					return current;
				}

				const next = [...current];
				const [item] = next.splice(index, 1);

				next.splice(targetIndex, 0, item);

				return next;
			});
		},
		[updateSidebarItems],
	);

	const resetSidebarItems = useCallback(async () => {
		// Same gate as `updateSidebarItems`. "Reset" writes the defaults on
		// purpose, but only as something the user ASKED for after seeing their
		// real settings — not as the accident of a failed read.
		const previous = preferenceWriteBasis(
			isSuccess,
			queryClient.getQueryData(SIDEBAR_NAVIGATION_QUERY_KEY) as
				| SidebarItemPreference[]
				| undefined,
			DEFAULT_SIDEBAR_ITEMS,
		);
		if (previous === null) {
			throw new PreferenceNotReadError("sidebar");
		}

		await writePreference({
			previous,
			next: DEFAULT_SIDEBAR_ITEMS,
			setCache: (value) =>
				queryClient.setQueryData(SIDEBAR_NAVIGATION_QUERY_KEY, value),
			save: saveSidebarItems,
		});
	}, [queryClient, isSuccess]);

	return {
		canEditSidebarItems: isSuccess,
		defaultHref,
		isLoading,
		isSidebarError: isError,
		retrySidebarItems: refetch,
		moveItem,
		resetSidebarItems,
		resolvedSidebarItems,
		setItemVisibility,
		sidebarItems,
		visibleSidebarItems,
	};
}
