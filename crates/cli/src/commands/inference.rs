//! `aghub-cli inference <list|get|add|update|delete|key>` — inventory CRUD over
//! the shared `InferenceProviderStore` (SQLite metadata + OS-keyring API keys).
//!
//! This is a thin CLI surface over `aghub_inference`: it builds the same
//! `CreateInferenceProvider` / `UpdateInferenceProvider` inputs the HTTP API
//! builds and calls the same `InferenceProviderRepository` methods, against the
//! same `app_data_dir` store. The keyring backend is shared, so a key stored by
//! the desktop is readable here and vice-versa. API keys are write-only: they
//! are resolved from a flag/stdin/env and never printed back.
//
// ponytail: print the `InferenceProvider` model + serde_json directly; do NOT
// drag the api DTO crate into the CLI.

use std::io::{IsTerminal, Read};

use aghub_inference::{
	CreateInferenceProvider, InferenceProvider, InferenceProviderFormat,
	InferenceProviderRepository, UpdateInferenceProvider,
};
use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use tabled::builder::Builder;
use tabled::settings::Style;

use crate::commands::inference_store;

/// Env var the API key falls back to when neither `--api-key` nor piped stdin
/// supplies one. Never passed on argv, where it would leak into the process
/// table / shell history.
const API_KEY_ENV: &str = "AGHUB_INFERENCE_API_KEY";

/// Actions for the `inference` subcommand group.
#[derive(Subcommand)]
pub enum InferenceAction {
	/// List all inference providers.
	List {
		#[arg(long)]
		json: bool,
	},
	/// Show one provider by id.
	Get {
		id: String,
		#[arg(long)]
		json: bool,
	},
	/// Add a provider. The API key is resolved from `--api-key`, else piped
	/// stdin, else `$AGHUB_INFERENCE_API_KEY` — never echoed back.
	Add {
		#[arg(long = "latin-name")]
		latin_name: String,
		#[arg(long = "display-name")]
		display_name: String,
		#[arg(long, value_parser = parse_format)]
		format: InferenceProviderFormat,
		#[arg(long = "api-base-url")]
		api_base_url: String,
		#[arg(long)]
		preset: Option<String>,
		/// The API key. Prefer stdin or `$AGHUB_INFERENCE_API_KEY` so it stays
		/// off argv.
		#[arg(long = "api-key")]
		api_key: Option<String>,
		/// Model name (repeatable).
		#[arg(long = "model")]
		models: Vec<String>,
		#[arg(long)]
		json: bool,
	},
	/// Update a provider's metadata (and optionally its API key).
	Update {
		id: String,
		#[arg(long = "latin-name")]
		latin_name: Option<String>,
		#[arg(long = "display-name")]
		display_name: Option<String>,
		#[arg(long, value_parser = parse_format)]
		format: Option<InferenceProviderFormat>,
		#[arg(long = "api-base-url")]
		api_base_url: Option<String>,
		#[arg(long)]
		preset: Option<String>,
		/// Replace the API key. Resolved from this flag only (stdin/env are not
		/// consulted on update — pass it explicitly).
		#[arg(long = "api-key")]
		api_key: Option<String>,
		/// Replace the model list (repeatable). Omit to leave models unchanged.
		#[arg(long = "model")]
		models: Vec<String>,
		#[arg(long)]
		json: bool,
	},
	/// Delete a provider and its API key. Destructive: needs `--yes`.
	Delete {
		id: String,
		#[arg(short = 'y', long = "yes")]
		yes: bool,
		#[arg(long)]
		json: bool,
	},
	/// Report whether an API key is stored for a provider (prints the masked
	/// preview only; NEVER the raw key).
	Key { id: String },
}

/// clap value parser for `--format`, reusing the canonical FromStr so the CLI
/// accepts the exact same spellings/aliases as the API and store.
fn parse_format(value: &str) -> Result<InferenceProviderFormat, String> {
	value
		.parse()
		.map_err(|e: aghub_inference::InferenceProviderError| e.to_string())
}

/// Dispatch an `inference` subcommand action.
pub fn execute(action: &InferenceAction) -> Result<()> {
	let store = inference_store();
	match action {
		InferenceAction::List { json } => list(&store, *json),
		InferenceAction::Get { id, json } => get(&store, id, *json),
		InferenceAction::Add {
			latin_name,
			display_name,
			format,
			api_base_url,
			preset,
			api_key,
			models,
			json,
		} => add(
			&store,
			AddArgs {
				latin_name,
				display_name,
				format: *format,
				api_base_url,
				preset: preset.as_deref(),
				api_key: api_key.as_deref(),
				models,
				json: *json,
			},
		),
		InferenceAction::Update {
			id,
			latin_name,
			display_name,
			format,
			api_base_url,
			preset,
			api_key,
			models,
			json,
		} => update(
			&store,
			id,
			UpdateArgs {
				latin_name: latin_name.as_deref(),
				display_name: display_name.as_deref(),
				format: *format,
				api_base_url: api_base_url.as_deref(),
				preset: preset.as_deref(),
				api_key: api_key.as_deref(),
				models,
				json: *json,
			},
		),
		InferenceAction::Delete { id, yes, json } => {
			delete(&store, id, *yes, *json)
		}
		InferenceAction::Key { id } => key(&store, id),
	}
}

// ─────────────────────────────── list / get ────────────────────────────────

fn list(store: &impl InferenceProviderRepository, json: bool) -> Result<()> {
	let providers = store.list().map_err(|e| anyhow!(e.to_string()))?;

	if json {
		println!("{}", serde_json::to_string_pretty(&providers)?);
		return Ok(());
	}

	if providers.is_empty() {
		println!("No inference providers.");
		return Ok(());
	}

	let mut builder = Builder::default();
	builder.push_record(["ID", "LATIN", "DISPLAY", "FORMAT", "BASE URL"]);
	for p in &providers {
		builder.push_record([
			p.id.clone(),
			p.latin_name.clone(),
			p.display_name.clone(),
			p.format.to_string(),
			p.api_base_url.clone(),
		]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	Ok(())
}

fn get(
	store: &impl InferenceProviderRepository,
	id: &str,
	json: bool,
) -> Result<()> {
	let provider = store.get(id).map_err(|e| anyhow!(e.to_string()))?;
	print_provider(&provider, json)
}

// ─────────────────────────────── add / update ──────────────────────────────

struct AddArgs<'a> {
	latin_name: &'a str,
	display_name: &'a str,
	format: InferenceProviderFormat,
	api_base_url: &'a str,
	preset: Option<&'a str>,
	api_key: Option<&'a str>,
	models: &'a [String],
	json: bool,
}

fn add(store: &impl InferenceProviderRepository, args: AddArgs) -> Result<()> {
	let api_key = resolve_api_key(args.api_key)?;
	let input = CreateInferenceProvider {
		latin_name: args.latin_name.to_string(),
		display_name: args.display_name.to_string(),
		format: args.format,
		api_base_url: args.api_base_url.to_string(),
		preset: args.preset.map(ToString::to_string),
		api_key,
		models: args.models.to_vec(),
	};
	let provider = store.create(input).map_err(|e| anyhow!(e.to_string()))?;
	print_provider(&provider, args.json)
}

struct UpdateArgs<'a> {
	latin_name: Option<&'a str>,
	display_name: Option<&'a str>,
	format: Option<InferenceProviderFormat>,
	api_base_url: Option<&'a str>,
	preset: Option<&'a str>,
	api_key: Option<&'a str>,
	models: &'a [String],
	json: bool,
}

fn update(
	store: &impl InferenceProviderRepository,
	id: &str,
	args: UpdateArgs,
) -> Result<()> {
	let input = UpdateInferenceProvider {
		latin_name: args.latin_name.map(ToString::to_string),
		display_name: args.display_name.map(ToString::to_string),
		format: args.format,
		api_base_url: args.api_base_url.map(ToString::to_string),
		// `--preset ""` clears the preset; any other value sets it.
		preset: args.preset.map(|value| {
			if value.is_empty() {
				None
			} else {
				Some(value.to_string())
			}
		}),
		api_key: args.api_key.map(ToString::to_string),
		// An empty repeatable flag means "leave models alone"; a non-empty one
		// replaces the whole list.
		models: if args.models.is_empty() {
			None
		} else {
			Some(args.models.to_vec())
		},
	};
	let provider = store
		.update(id, input)
		.map_err(|e| anyhow!(e.to_string()))?;
	print_provider(&provider, args.json)
}

// ──────────────────────────────── delete / key ─────────────────────────────

fn delete<C: aghub_inference::CredentialStore>(
	store: &aghub_inference::InferenceProviderStore<C>,
	id: &str,
	yes: bool,
	json: bool,
) -> Result<()> {
	if !yes {
		bail!(
			"refusing to delete inference provider '{id}' without --yes \
			 (this also removes its stored API key)"
		);
	}
	// Shared use case (same as the API delete route): tear down every agent
	// reference (Claude/Codex/OpenCode bindings + config), THEN remove the
	// provider — never just `store.delete`, which would leave agent configs
	// pointing at a deleted provider.
	let provider = store.get(id).map_err(|e| anyhow!(e.to_string()))?;
	aghub_inference::delete_provider_cascade(store, &provider)
		.map_err(|e| anyhow!(e.to_string()))?;
	print_provider(&provider, json)
}

fn key(store: &impl InferenceProviderRepository, id: &str) -> Result<()> {
	// `get_api_key` first verifies the provider exists, then reads the keyring.
	let stored = store.get_api_key(id).map_err(|e| anyhow!(e.to_string()))?;
	let provider = store.get(id).map_err(|e| anyhow!(e.to_string()))?;
	// NEVER print the raw key — only the masked preview + a presence flag.
	println!(
		"{}\t{}\tstored={}",
		provider.id,
		provider.masked_api_key,
		stored.is_some()
	);
	Ok(())
}

// ──────────────────────────────── helpers ──────────────────────────────────

/// Resolve the API key for `create`: `--api-key`, else piped stdin, else
/// `$AGHUB_INFERENCE_API_KEY`, else a clear error. The raw key never goes on
/// argv beyond the explicit flag; prefer stdin/env.
fn resolve_api_key(flag: Option<&str>) -> Result<String> {
	if let Some(key) = flag {
		return Ok(key.to_string());
	}

	// Only read stdin when it is piped, so an interactive run errors fast
	// instead of blocking on a tty.
	let stdin = std::io::stdin();
	if !stdin.is_terminal() {
		let mut buf = String::new();
		stdin.lock().read_to_string(&mut buf)?;
		let trimmed = buf.trim();
		if !trimmed.is_empty() {
			return Ok(trimmed.to_string());
		}
	}

	if let Ok(key) = std::env::var(API_KEY_ENV) {
		let trimmed = key.trim();
		if !trimmed.is_empty() {
			// Trim like the stdin path — an env var assigned with a trailing
			// newline must not store the newline into the keyring.
			return Ok(trimmed.to_string());
		}
	}

	Err(anyhow!(
		"missing api key: pass --api-key, pipe it on stdin, or set \
		 ${API_KEY_ENV}"
	))
}

/// Print a provider as pretty JSON (`--json`) or a key/value block. Neither
/// path can leak the raw key — the model only carries `masked_api_key`.
fn print_provider(provider: &InferenceProvider, json: bool) -> Result<()> {
	if json {
		println!("{}", serde_json::to_string_pretty(provider)?);
		return Ok(());
	}
	println!("id:           {}", provider.id);
	println!("latin_name:   {}", provider.latin_name);
	println!("display_name: {}", provider.display_name);
	println!("format:       {}", provider.format);
	println!("api_base_url: {}", provider.api_base_url);
	if let Some(preset) = &provider.preset {
		println!("preset:       {preset}");
	}
	println!("masked_key:   {}", provider.masked_api_key);
	if !provider.models.is_empty() {
		println!("models:       {}", provider.models.join(", "));
	}
	Ok(())
}
