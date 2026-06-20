import type { Store } from "@tauri-apps/plugin-store";

export async function migrateV6ToV7(store: Store): Promise<void> {
	const sidebarItems = await store.get<
		Array<{ id: string; visible: boolean }>
	>("sidebarItems");
	if (!sidebarItems) return;

	const next = sidebarItems.filter((item) => item.id !== "sources");
	await store.set("sidebarItems", next);
}
