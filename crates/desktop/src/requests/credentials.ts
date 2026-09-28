import {
	mutationOptions,
	type QueryClient,
	queryOptions,
} from "@tanstack/react-query";
import type {
	CreateCredentialRequest,
	CredentialResponse,
	SourceCredentialBindingRequest,
	SourceCredentialBindingResponse,
} from "../generated/dto";
import type { ApiClient } from "./client";
import { queryKeys } from "./keys.ts";

interface CredentialsQueryParams {
	api: ApiClient;
	enabled: boolean;
}

export function credentialsListQueryOptions({
	api,
	enabled,
}: CredentialsQueryParams) {
	return queryOptions({
		queryKey: queryKeys.credentials.list(),
		queryFn: () => api.credentials.list(),
		enabled,
	});
}

export function sourceCredentialBindingsQueryOptions({
	api,
	enabled,
}: CredentialsQueryParams) {
	return queryOptions({
		queryKey: queryKeys.credentials.sourceBindings(),
		queryFn: () => api.credentials.listSourceBindings(),
		enabled,
	});
}

/// EVERY credential mutation (delete, bind, AND create) changes which token a
/// source resolves to — deleting one prunes its source bindings server side,
/// and `resolve.rs` falls back to a credential whose NAME matches the host, so
/// creating `github.com` authenticates every github source. Source diffs and
/// update checks are computed with that token, so all three mutations
/// invalidate them here; otherwise they keep serving the pre-change answer and
/// the UI looks like nothing changed.
///
/// NOTHING is awaited past marking stale: these are slow HTTP round trips (a
/// source diff clones behind a 120s timeout; the credential list carries a 10s
/// timeout plus a retry), and awaiting a refetch would hold the dialog pending
/// after the write succeeded. Update checks are not refetched at all — that
/// refetches EVERY source, the price `invalidateSkillQueries` also declines to
/// pay. They stay stale until the user asks, which is the honest state.
async function invalidateSourceCredentialAnswers(queryClient: QueryClient) {
	for (const queryKey of [
		queryKeys.credentials.all(),
		queryKeys.skills.sources.all(),
		queryKeys.skills.updateChecksAll(),
	]) {
		await queryClient.invalidateQueries({
			queryKey,
			refetchType: "none",
		});
	}
	for (const queryKey of [
		queryKeys.credentials.all(),
		queryKeys.skills.sources.all(),
	]) {
		void queryClient.refetchQueries({ queryKey, type: "active" });
	}
}

interface CreateCredentialMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: CredentialResponse) => void | Promise<void>;
	onError?: (error: Error) => void;
}

export function createCredentialMutationOptions({
	api,
	queryClient,
	onSuccess,
	onError,
}: CreateCredentialMutationParams) {
	return mutationOptions({
		mutationFn: (body: CreateCredentialRequest) =>
			api.credentials.create(body),
		onSuccess: async (data) => {
			await invalidateSourceCredentialAnswers(queryClient);
			await onSuccess?.(data);
		},
		onError,
	});
}

interface BindSourceCredentialMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: SourceCredentialBindingResponse) => void | Promise<void>;
	onError?: (error: Error) => void;
}

export function bindSourceCredentialMutationOptions({
	api,
	queryClient,
	onSuccess,
	onError,
}: BindSourceCredentialMutationParams) {
	return mutationOptions({
		mutationFn: (body: SourceCredentialBindingRequest) =>
			api.credentials.bindSource(body),
		onSuccess: async (data) => {
			await invalidateSourceCredentialAnswers(queryClient);
			await onSuccess?.(data);
		},
		onError,
	});
}

interface DeleteCredentialMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: () => void | Promise<void>;
}

export function deleteCredentialMutationOptions({
	api,
	queryClient,
	onSuccess,
}: DeleteCredentialMutationParams) {
	return mutationOptions({
		mutationFn: (id: string) => api.credentials.delete(id),
		onSuccess: async () => {
			await invalidateSourceCredentialAnswers(queryClient);
			await onSuccess?.();
		},
	});
}
