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
/// in invocation order regardless of whether a script matched.
pub(crate) struct MockRunner {
	scripts: HashMap<(String, Vec<String>), CommandOutput>,
	calls: RefCell<Vec<RecordedCall>>,
}

impl MockRunner {
	pub(crate) fn new() -> Self {
		Self {
			scripts: HashMap::new(),
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

	/// All recorded calls, in invocation order.
	pub(crate) fn calls(&self) -> Vec<RecordedCall> {
		self.calls.borrow().clone()
	}
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
		match self.scripts.get(&key) {
			Some(out) => Ok(out.clone()),
			None => Err(RunError::Spawn(format!(
				"MockRunner: no script for {program} {args:?}"
			))),
		}
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
