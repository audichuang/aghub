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

export async function invalidateCredentialQueries(queryClient: QueryClient) {
	await queryClient.invalidateQueries({
		queryKey: queryKeys.credentials.all(),
	});
}

/// EVERY credential mutation changes which token a source can resolve to, so
/// all three go through here. Deleting one prunes its source bindings server
/// side; binding one repoints a source; and CREATING one is not neutral either
/// — `resolve.rs` falls back to a credential whose NAME matches the host, so a
/// credential called `github.com` starts authenticating every github source the
/// moment it exists. Source diffs and update checks are computed WITH that
/// token, so they stop being true at that instant; without this they keep
/// serving the pre-change answer (diff 30-60s, update checks 10 min) and the UI
/// looks like nothing changed.
///
/// Marked stale WITHOUT awaiting a refetch, then refetched fire-and-forget: a
/// source diff CLONES the repo behind a 120s timeout, so awaiting it would hold
/// the mutation pending long after the write succeeded and the bind dialog
/// would sit there looking hung. Update checks are not refetched at all —
/// re-running them refetches EVERY source, the price `invalidateSkillQueries`
/// also declines to pay.
async function invalidateSourceCredentialAnswers(queryClient: QueryClient) {
	await invalidateCredentialQueries(queryClient);
	for (const queryKey of [
		queryKeys.skills.sources.all(),
		queryKeys.skills.updateChecksAll(),
	]) {
		await queryClient.invalidateQueries({
			queryKey,
			refetchType: "none",
		});
	}
	void queryClient.refetchQueries({
		queryKey: queryKeys.skills.sources.all(),
		type: "active",
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
