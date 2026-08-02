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
import { queryKeys } from "./keys";

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

export async function invalidateCredentialQueries(queryClient: QueryClient) {
	await queryClient.invalidateQueries({
		queryKey: queryKeys.credentials.all(),
	});
}

/// For the credential mutations that change which token a SOURCE resolves to —
/// binding one, or deleting one (the backend prunes that credential's source
/// bindings with it). Source diffs and update checks are computed WITH that
/// token, so they stop being true the moment it changes; without this they keep
/// serving the pre-change answer (diff 30-60s, update checks 10 min) and the UI
/// looks like the credential still works.
///
/// Source diffs refetch (one source, and the user is usually looking at it);
/// update checks are only marked stale, because re-running them refetches EVERY
/// source over the network — the same price the skill mutations decline to pay.
async function invalidateSourceCredentialAnswers(queryClient: QueryClient) {
	await invalidateCredentialQueries(queryClient);
	await queryClient.invalidateQueries({
		queryKey: queryKeys.skills.sources.all(),
	});
	await queryClient.invalidateQueries({
		queryKey: queryKeys.skills.updateChecksAll(),
		refetchType: "none",
	});
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
			await invalidateCredentialQueries(queryClient);
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
