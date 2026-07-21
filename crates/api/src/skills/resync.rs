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
	match error {
		ResyncError::NotInstalled => SafeResyncError {
			message: "Skill is locked but no installed copy was found",
			code: "SKILL_NOT_INSTALLED",
			status: Status::NotFound,
		},
		ResyncError::Renamed { .. } => SafeResyncError {
			message: "Source skill was renamed",
			code: "SKILL_RENAMED",
			status: Status::BadRequest,
		},
		ResyncError::Parse(_) => SafeResyncError {
			message: "Failed to parse synced skill",
			code: "SKILL_PARSE_FAILED",
			status: Status::BadRequest,
		},
		ResyncError::OutOfTree(_) => SafeResyncError {
			message: "Refusing to sync out-of-tree target",
			code: "SKILL_TARGET_OUT_OF_TREE",
			status: Status::BadRequest,
		},
		ResyncError::Hash(_) | ResyncError::Swap(_) => SafeResyncError {
			message: "Failed to sync skill",
			code: "SKILL_SYNC_ERROR",
			status: Status::InternalServerError,
		},
		ResyncError::LockUpdate(_) => SafeResyncError {
			message: "Failed to update skill lock after sync",
			code: "SKILL_LOCK_ERROR",
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
		];

		for error in cases {
			let mapped = safe_resync_error(&error);
			assert!(
				!mapped.message.contains(sentinel),
				"safe API error leaked internal path for {error:?}",
			);
			assert!(!mapped.code.is_empty());
		}
	}
}
