use aghub_core::skills::resync::ResyncError;
use rocket::http::Status;

/// Public API projection of an internal resync failure.
///
/// Core errors may contain absolute target, staging, or lock paths. Keep that
/// diagnostic detail inside the process and expose only stable, path-free
/// messages and codes at the HTTP boundary.
pub(crate) struct SafeResyncError {
	pub(crate) message: &'static str,
	pub(crate) code: &'static str,
	pub(crate) status: Status,
}

pub(crate) fn safe_resync_error(error: &ResyncError) -> SafeResyncError {
	// The CODE is the shared classification — one table in core, so a CLI row
	// and an HTTP body name the same failure the same way. The MESSAGE and the
	// HTTP status stay here: only this surface owes a path-free wording, and
	// only this surface has a status to pick.
	let code = aghub_core::skills::resync::resync_error_code(error);
	match error {
		ResyncError::Locked(_) => SafeResyncError {
			message: "Another aghub process is mutating skills; retry shortly",
			code,
			status: Status::Conflict,
		},
		ResyncError::StaleFetch(_) => SafeResyncError {
			message:
				"The skill's source changed while this sync was fetching; \
			          nothing was written. Re-run to use the current source",
			code,
			status: Status::Conflict,
		},
		ResyncError::NotInstalled => SafeResyncError {
			message: "Skill is locked but no installed copy was found",
			code,
			status: Status::NotFound,
		},
		// Both shipped callers intercept `Renamed` in an EARLIER arm to build the
		// name-carrying message, so nothing in production reaches this one — and
		// a caller that ever does must still name the failure the way every
		// other surface does. This used to hold a hand-written `SKILL_RENAMED`
		// literal defended by a claim about a shipped client matching on it; no
		// shipped path can observe it, and the literal matched no other surface.
		ResyncError::Renamed { .. } => SafeResyncError {
			message: "Source skill was renamed",
			code,
			status: Status::BadRequest,
		},
		ResyncError::Parse(_) => SafeResyncError {
			message: "Failed to parse synced skill",
			code,
			status: Status::BadRequest,
		},
		// The findings themselves stay in the server log: `message` is
		// `&'static str` by design, and the surface that should render a rule
		// list is a UI, not an error string.
		ResyncError::Audit(_) => SafeResyncError {
			message: "Skill update was refused by the security audit",
			code,
			status: Status::BadRequest,
		},
		ResyncError::OutOfTree(_) => SafeResyncError {
			message: "Refusing to sync out-of-tree target",
			code,
			status: Status::BadRequest,
		},
		ResyncError::Hash(_) | ResyncError::Swap(_) => SafeResyncError {
			message: "Failed to sync skill",
			code,
			status: Status::InternalServerError,
		},
		ResyncError::LockUpdate(_) => SafeResyncError {
			message: "Failed to update skill lock after sync",
			code,
			status: Status::InternalServerError,
		},
	}
}

#[cfg(test)]
mod tests {
	use super::safe_resync_error;
	use aghub_core::skills::resync::ResyncError;

	#[test]
	fn resync_error_mapping_never_exposes_internal_paths() {
		let sentinel = "/private/tmp/aghub-secret-target";
		let cases = [
			ResyncError::Parse(sentinel.to_string()),
			ResyncError::OutOfTree(sentinel.to_string()),
			ResyncError::Hash(sentinel.to_string()),
			ResyncError::Swap(sentinel.to_string()),
			ResyncError::LockUpdate(sentinel.to_string()),
			ResyncError::Locked(sentinel.to_string()),
			ResyncError::StaleFetch(sentinel.to_string()),
			// Carries a caller-supplied name, and used to be the one arm with a
			// hand-written code of its own.
			ResyncError::Renamed {
				new_name: sentinel.to_string(),
			},
			ResyncError::NotInstalled,
		];

		for error in cases {
			let mapped = safe_resync_error(&error);
			assert!(
				!mapped.message.contains(sentinel),
				"safe API error leaked internal path for {error:?}",
			);
			// Every arm's code is core's shared classification. A literal
			// written out here would be a second table, and the two would
			// drift — the CLI reads the core one.
			assert_eq!(
				mapped.code,
				aghub_core::skills::resync::resync_error_code(&error),
				"the API must publish core's code for this variant",
			);
		}
	}
}
