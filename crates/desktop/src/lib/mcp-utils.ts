import type { EnvVar } from "../components/env-editor";
import type { HttpHeader } from "../components/http-header-editor";
import type { TransportDto } from "../generated/dto";
import { keyPairToObject } from "./key-pair-utils.ts";

// Static regex to avoid re-compilation on every call
const WHITESPACE_REGEX = /\s+/;

export type McpImportTransportType = "stdio" | "sse" | "streamable_http";

// Aliases an agent's own config may use for streamable HTTP. Mirrors
// `HTTP_READ_ALIASES` in `crates/agents/src/format/json_map.rs` — keep the two
// in step; a spelling missing here silently imports as SSE.
const HTTP_TYPE_ALIASES = [
	"streamable_http",
	"http",
	"streamable-http",
	"streamableHttp",
];

export interface McpImportServerConfig {
	/** Widened past `McpImportTransportType`: the JSON is somebody else's
	 * config, so it carries the agent's native spelling (e.g. `"http"`). */
	type?: string;
	command?: string;
	args?: string[];
	env?: Record<string, string>;
	url?: string;
	headers?: Record<string, string>;
	timeout?: number;
}

export interface McpImportJson {
	mcpServers?: Record<string, McpImportServerConfig>;
}

// Shell-style argv round-trip for the single-line args field. `formatArgs` is
// the inverse of `parseArgs`, so opening an existing server in the edit form
// and saving it back must not change its argv — a plain `split(/\s+/)` used to
// shred any arg containing a space (`/p q` → `/p`, `q`) even when the user
// never touched the field.
const NEEDS_QUOTING_REGEX = /[\s"'\\]/;

export function parseArgs(input: string): string[] {
	const args: string[] = [];
	let current = "";
	let quote: '"' | "'" | null = null;
	let started = false;

	for (let i = 0; i < input.length; i++) {
		const ch = input[i];

		if (ch === "\\" && quote !== "'" && i + 1 < input.length) {
			current += input[++i];
			started = true;
			continue;
		}

		if (quote) {
			if (ch === quote) {
				quote = null;
			} else {
				current += ch;
			}
			continue;
		}

		if (ch === '"' || ch === "'") {
			quote = ch;
			started = true;
			continue;
		}

		if (WHITESPACE_REGEX.test(ch)) {
			if (started) {
				args.push(current);
				current = "";
				started = false;
			}
			continue;
		}

		current += ch;
		started = true;
	}

	if (started) {
		args.push(current);
	}

	return args;
}

export function formatArgs(args: string[]): string {
	return args
		.map((arg) =>
			arg === "" || NEEDS_QUOTING_REGEX.test(arg)
				? `"${arg.replace(/(["\\])/g, "\\$1")}"`
				: arg,
		)
		.join(" ");
}

export function buildTransportFromForm(
	transportType: "stdio" | "sse" | "streamable_http",
	data: {
		command?: string;
		args?: string;
		envVars?: EnvVar[];
		url?: string;
		httpHeaders?: HttpHeader[];
		timeout?: string;
	},
): TransportDto | undefined {
	const timeoutNum = data.timeout ? Number.parseInt(data.timeout, 10) : null;

	if (transportType === "stdio") {
		const argsArray = parseArgs(data.args ?? "");
		const envRecord: Record<string, string> | null =
			data.envVars && data.envVars.length > 0
				? keyPairToObject(data.envVars)
				: null;

		return {
			type: "stdio",
			command: data.command?.trim() ?? "",
			args: argsArray,
			env: envRecord,
			timeout: timeoutNum,
		};
	}

	const headersRecord: Record<string, string> | null =
		data.httpHeaders && data.httpHeaders.length > 0
			? keyPairToObject(data.httpHeaders)
			: null;

	return {
		type: transportType,
		url: data.url?.trim() ?? "",
		headers: headersRecord,
		timeout: timeoutNum,
	};
}

export function capitalize(str: string): string {
	return str.charAt(0).toUpperCase() + str.slice(1).toLowerCase();
}

export function getImportedMcpTransportType(
	config: McpImportServerConfig,
): McpImportTransportType | null {
	if (config.command) {
		return "stdio";
	}

	if (config.url) {
		return config.type && HTTP_TYPE_ALIASES.includes(config.type)
			? "streamable_http"
			: "sse";
	}

	return null;
}

export function toMcpImportServerConfig(
	transport: TransportDto,
): McpImportServerConfig {
	if (transport.type === "stdio") {
		return {
			command: transport.command,
			...(transport.args.length > 0 ? { args: transport.args } : {}),
			...(transport.env ? { env: transport.env } : {}),
			...(transport.timeout !== null
				? { timeout: transport.timeout }
				: {}),
		};
	}

	return {
		url: transport.url,
		...(transport.type === "streamable_http"
			? { type: transport.type }
			: {}),
		...(transport.headers ? { headers: transport.headers } : {}),
		...(transport.timeout !== null ? { timeout: transport.timeout } : {}),
	};
}

export function serializeMcpImportJson(
	name: string,
	transport: TransportDto,
): string {
	return JSON.stringify(
		{
			mcpServers: {
				[name]: toMcpImportServerConfig(transport),
			},
		},
		null,
		2,
	);
}
