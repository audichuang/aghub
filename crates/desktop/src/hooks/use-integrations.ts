import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo } from "react";
import type { CodeEditorType } from "../generated/dto";
import {
	PreferenceNotReadError,
	preferenceWriteBasis,
	writePreference,
} from "../lib/preference-write";
import {
	getIntegrationPreferences,
	saveIntegrationPreferences,
} from "../lib/store";
import type { IntegrationPreferences } from "../lib/store/types";
import { codeEditorsQueryOptions } from "../requests/integrations";
import { useApi } from "./use-api";

const INTEGRATION_PREFERENCES_KEY = "integration-preferences";

export function useCurrentCodeEditor() {
	const queryClient = useQueryClient();
	const api = useApi();
	const {
		data: codeEditors,
		isLoading: isLoadingEditors,
		isError: isEditorsError,
		refetch: retryCodeEditors,
	} = useQuery({
		...codeEditorsQueryOptions({ api }),
	});
	const {
		data: preferences,
		isLoading: isLoadingPreferences,
		isSuccess: preferencesRead,
	} = useQuery({
		queryKey: [INTEGRATION_PREFERENCES_KEY],
		queryFn: getIntegrationPreferences,
	});

	// A RENDERING fallback chain follows (preference, else the first installed
	// editor). A failed preference read lands on the same branch as "no
	// preference saved", so the write below must not take what is on screen as
	// the user's choice — picking an editor would otherwise overwrite a
	// preference the app never managed to read.
	const preferredEditor = preferences?.codeEditor;

	const selectedEditor = useMemo(() => {
		if (preferredEditor) {
			return preferredEditor;
		}

		return codeEditors?.find((editor) => editor.installed)?.id as
			| CodeEditorType
			| undefined;
	}, [codeEditors, preferredEditor]);

	const currentEditor = useMemo(
		() => codeEditors?.find((editor) => editor.id === selectedEditor),
		[codeEditors, selectedEditor],
	);

	const setCurrentEditor = useCallback(
		async (editor: CodeEditorType | undefined) => {
			const basis = preferenceWriteBasis(
				preferencesRead,
				queryClient.getQueryData([INTEGRATION_PREFERENCES_KEY]) as
					| IntegrationPreferences
					| undefined,
				{},
			);
			if (basis === null) {
				throw new PreferenceNotReadError("integrationPreferences");
			}

			await writePreference({
				previous: basis,
				// Spread, not replace: `IntegrationPreferences` is a bag, and
				// rebuilding it from one field drops every other key it grows.
				next: { ...basis, codeEditor: editor },
				setCache: (value) =>
					queryClient.setQueryData(
						[INTEGRATION_PREFERENCES_KEY],
						value,
					),
				save: saveIntegrationPreferences,
			});
		},
		[queryClient, preferencesRead],
	);

	return {
		codeEditors,
		currentEditor,
		isEditorsError,
		retryCodeEditors,
		selectedEditor,
		setCurrentEditor,
		isLoading: isLoadingEditors || isLoadingPreferences,
	};
}
