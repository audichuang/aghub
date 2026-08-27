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

use std::io::Read;

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
#[derive(Subcommand, Clone)]
pub enum InferenceAction {
	/// List all inference providers.
	List,
	/// Show one provider by id.
	Get {
		/// Provider id from `inference list` (the ID column)
		id: String,
	},
	/// Add a provider. The API key is resolved from `--api-key`, else piped
	/// stdin, else `$AGHUB_INFERENCE_API_KEY` — never echoed back.
	Add {
		/// Machine-readable identifier, e.g. `my-openrouter`. ASCII; this is
		/// what agent configs reference.
		#[arg(long = "latin-name", value_name = "NAME")]
		latin_name: String,
		/// Human-readable label shown in the desktop UI
		#[arg(long = "display-name", value_name = "TEXT")]
		display_name: String,
		/// Wire protocol the endpoint speaks
		#[arg(long, value_parser = parse_format, value_name = FORMAT_VALUE_NAME)]
		format: InferenceProviderFormat,
		/// Endpoint root, e.g. `https://openrouter.ai/api/v1`
		#[arg(long = "api-base-url", value_name = "URL")]
		api_base_url: String,
		/// Optional named preset to seed defaults from (see the desktop
		/// provider presets); omit for a fully custom provider
		#[arg(long, value_name = "NAME")]
		preset: Option<String>,
		/// The API key. Pass `-` to read it from stdin, or set
		/// `$AGHUB_INFERENCE_API_KEY`, so it stays off argv.
		///
		/// stdin is read ONLY for `-`. Reading it whenever stdin was not a tty
		/// hung forever on the open, idle pipe a non-interactive harness leaves
		/// behind.
		#[arg(long = "api-key")]
		api_key: Option<String>,
		/// Model name (repeatable).
		#[arg(long = "model", value_name = "MODEL")]
		models: Vec<String>,
	},
	/// Update a provider's metadata (and optionally its API key).
	Update {
		/// Provider id from `inference list` (the ID column)
		id: String,
		/// New machine-readable identifier
		#[arg(long = "latin-name", value_name = "NAME")]
		latin_name: Option<String>,
		/// New human-readable label
		#[arg(long = "display-name", value_name = "TEXT")]
		display_name: Option<String>,
		/// New wire protocol
		#[arg(long, value_parser = parse_format, value_name = FORMAT_VALUE_NAME)]
		format: Option<InferenceProviderFormat>,
		/// New endpoint root
		#[arg(long = "api-base-url", value_name = "URL")]
		api_base_url: Option<String>,
		/// New preset name; pass an empty string to clear it
		#[arg(long, value_name = "NAME")]
		preset: Option<String>,
		/// Replace the API key. Resolved from this flag only (stdin/env are not
		/// consulted on update — pass it explicitly).
		#[arg(long = "api-key")]
		api_key: Option<String>,
		/// Replace the model list (repeatable). Omit to leave models unchanged.
		#[arg(long = "model", value_name = "MODEL")]
		models: Vec<String>,
	},
	/// Delete a provider and its API key. Destructive: needs `--yes`.
	Delete {
		/// Provider id from `inference list` (the ID column)
		id: String,
		/// Actually delete. Without it the command refuses.
		#[arg(short = 'y', long = "yes")]
		yes: bool,
	},
	/// Report whether an API key is stored for a provider (prints the masked
	/// preview only; NEVER the raw key).
	Key {
		/// Provider id from `inference list` (the ID column)
		id: String,
	},
}

/// `--format`'s placeholder, which clap prints in the usage line and in the
/// per-flag help. `InferenceProviderFormat` is not a clap `ValueEnum` — it is
/// the store's canonical type with its own `FromStr` aliases, and deriving a
/// parallel CLI enum would duplicate the very list that has to stay in step —
/// so the accepted values are surfaced here and in [`parse_format`]'s error
/// instead of via `[possible values: ...]`.
const FORMAT_VALUE_NAME: &str = "anthropic|openai_completions|openai_responses";

/// clap value parser for `--format`, reusing the canonical FromStr so the CLI
/// accepts the exact same spellings/aliases as the API and store.
fn parse_format(value: &str) -> Result<InferenceProviderFormat, String> {
	value
		.parse()
		.map_err(|e: aghub_inference::InferenceProviderError| {
			format!("{e} (expected one of: {FORMAT_VALUE_NAME})")
		})
}

/// Dispatch an `inference` subcommand action.
pub fn execute(action: &InferenceAction, json: bool) -> Result<()> {
	let store = inference_store();
	match action {
		InferenceAction::List => list(&store, json),
		InferenceAction::Get { id } => get(&store, id, json),
		InferenceAction::Add {
			latin_name,
			display_name,
			format,
			api_base_url,
			preset,
			api_key,
			models,
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
				json,
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
				json,
			},
		),
		InferenceAction::Delete { id, yes } => delete(&store, id, *yes, json),
		InferenceAction::Key { id } => key(&store, id, json),
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

fn key(
	store: &impl InferenceProviderRepository,
	id: &str,
	json: bool,
) -> Result<()> {
	// `get_api_key` first verifies the provider exists, then reads the keyring.
	let stored = store.get_api_key(id).map_err(|e| anyhow!(e.to_string()))?;
	let provider = store.get(id).map_err(|e| anyhow!(e.to_string()))?;
	// NEVER print the raw key — only the masked preview + a presence flag.
	// Both branches carry the same three fields; neither can reach the secret,
	// which lives only in `stored` and is reduced to a bool here.
	if json {
		println!(
			"{}",
			serde_json::to_string_pretty(&serde_json::json!({
				"id": provider.id,
				"maskedApiKey": provider.masked_api_key,
				"stored": stored.is_some(),
			}))?
		);
		return Ok(());
	}
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
	// `--api-key -` is the ONLY way to read stdin. It used to read stdin
	// whenever stdin was not a tty, which protected an interactive user but
	// created the agent failure mode: a harness that leaves an open, idle pipe
	// on stdin (the normal shape of non-interactive execution) made this block
	// to EOF forever — no prompt, no output, no diagnostic, just a hang.
	// Explicit opt-in keeps the key off argv without that trap.
	match flag {
		Some("-") => {
			let mut buf = String::new();
			std::io::stdin().lock().read_to_string(&mut buf)?;
			let trimmed = buf.trim();
			if trimmed.is_empty() {
				anyhow::bail!(
					"--api-key - was given but stdin was empty; pipe the key \
					 in, or pass it as --api-key <KEY>, or set \
					 ${API_KEY_ENV}"
				);
			}
			return Ok(trimmed.to_string());
		}
		Some(key) => return Ok(key.to_string()),
		None => {}
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
		"missing api key: pass --api-key <KEY>, pass `--api-key -` to read \
		 it from stdin, or set ${API_KEY_ENV}"
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
