//! Test-only support shared across this crate's modules (W2 + W3).
//!
//! `MockRunner` implements [`crate::ssh::CommandRunner`] by replaying scripted
//! [`crate::ssh::CommandOutput`] keyed on `(program, args)` and recording every
//! call so tests can assert what was invoked (e.g. that a guarded remote kill
//! was issued). It is `pub(crate)` so the W3 bring-up tests in this same crate
//! can reuse it without a real `ssh`.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::ssh::{ChildHandle, CommandOutput, CommandRunner, RunError};

/// A single recorded invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedCall {
	pub program: String,
	pub args: Vec<String>,
}

/// A scripted [`CommandRunner`] for unit tests.
///
/// Scripts are matched on the exact `(program, args)` tuple. Calls are recorded
/// in invocation order regardless of whether a script matched. A `default_for`
/// registration answers ANY call to that `program` that has no exact script —
/// used when a call's args embed a per-invocation value a test cannot predict
/// exactly (e.g. the install nonce threaded through the scp/finish commands),
/// so the test still exercises the full control flow and asserts on the
/// RECORDED call args afterward instead of an exact key match.
pub(crate) struct MockRunner {
	scripts: HashMap<(String, Vec<String>), CommandOutput>,
	defaults: HashMap<String, CommandOutput>,
	calls: RefCell<Vec<RecordedCall>>,
}

impl MockRunner {
	pub(crate) fn new() -> Self {
		Self {
			scripts: HashMap::new(),
			defaults: HashMap::new(),
			calls: RefCell::new(Vec::new()),
		}
	}

	/// Register a canned output for an exact `(program, args)` invocation.
	pub(crate) fn script(
		mut self,
		program: &str,
		args: &[&str],
		output: CommandOutput,
	) -> Self {
		let key = (
			program.to_string(),
			args.iter().map(|s| s.to_string()).collect(),
		);
		self.scripts.insert(key, output);
		self
	}

	/// Register a fallback output for any `program` call that has no exact
	/// `script`ed match. Exact matches always win over this.
	pub(crate) fn default_for(
		mut self,
		program: &str,
		output: CommandOutput,
	) -> Self {
		self.defaults.insert(program.to_string(), output);
		self
	}

	/// All recorded calls, in invocation order.
	pub(crate) fn calls(&self) -> Vec<RecordedCall> {
		self.calls.borrow().clone()
	}
}

/// Decode a standard (RFC 4648) base64 string back to raw bytes.
///
/// Test-only: production code never decodes on this side — the remote's own
/// `base64 -d` does that (see `ssh::build_bash_wrapped_cmd`). Tests use this
/// to recover the plaintext remote command from a recorded ssh argv so they
/// can assert on values (like a staged upload path) that only exist inside
/// the base64 payload.
fn base64_decode(encoded: &str) -> Vec<u8> {
	const TABLE: &[u8; 64] =
		b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	let value_of = |c: u8| -> u8 {
		TABLE
			.iter()
			.position(|&t| t == c)
			.expect("invalid base64 character in recorded argv") as u8
	};
	let bytes = encoded.as_bytes();
	let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
	for chunk in bytes.chunks(4) {
		let c0 = value_of(chunk[0]);
		let c1 = value_of(chunk[1]);
		out.push((c0 << 2) | (c1 >> 4));
		if chunk.len() > 2 && chunk[2] != b'=' {
			let c2 = value_of(chunk[2]);
			out.push((c1 << 4) | (c2 >> 2));
			if chunk.len() > 3 && chunk[3] != b'=' {
				let c3 = value_of(chunk[3]);
				out.push((c2 << 6) | c3);
			}
		}
	}
	out
}

/// Recover the plaintext remote command wrapped by `ssh::build_ssh_args`'s
/// fixed `bash -lc 'eval "$(printf %s <base64> | base64 -d)"'` bash wrapper.
///
/// `ssh_args` is a full recorded ssh argv (the last element is the wrapped
/// command). Used to assert on a value (e.g. the staged install path) that
/// is only visible inside the base64 payload, so tests can pin an EXACT
/// pairing between two separate recorded calls instead of "some ssh call
/// ran".
pub(crate) fn decode_wrapped_remote_cmd(ssh_args: &[String]) -> String {
	let wrapped = ssh_args
		.last()
		.expect("ssh argv must have a trailing wrapped command");
	let prefix = "bash -lc 'eval \"$(printf %s ";
	let suffix = " | base64 -d)\"'";
	let encoded = wrapped
		.strip_prefix(prefix)
		.and_then(|s| s.strip_suffix(suffix))
		.unwrap_or_else(|| {
			panic!("unexpected wrapped-command shape: {wrapped}")
		});
	String::from_utf8(base64_decode(encoded))
		.expect("decoded remote command must be UTF-8")
}

impl CommandRunner for MockRunner {
	fn run(
		&self,
		program: &str,
		args: &[String],
	) -> Result<CommandOutput, RunError> {
		self.calls.borrow_mut().push(RecordedCall {
			program: program.to_string(),
			args: args.to_vec(),
		});
		let key = (program.to_string(), args.to_vec());
		if let Some(out) = self.scripts.get(&key) {
			return Ok(out.clone());
		}
		if let Some(out) = self.defaults.get(program) {
			return Ok(out.clone());
		}
		Err(RunError::Spawn(format!(
			"MockRunner: no script for {program} {args:?}"
		)))
	}

	fn spawn(
		&self,
		program: &str,
		args: &[String],
	) -> Result<ChildHandle, RunError> {
		self.calls.borrow_mut().push(RecordedCall {
			program: program.to_string(),
			args: args.to_vec(),
		});
		Err(RunError::Spawn(
			"MockRunner: spawn is not supported in tests".to_string(),
		))
	}
}
