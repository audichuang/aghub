use aghub_core::skills::resync::ResyncError;
use rocket::http::Status;

/// Public API projection of an internal resync failure.
///
/// Core errors may contain absolute target, staging, or lock paths. Keep that
/// diagnostic detail inside the process and expose only stable, path-free
/// messages at the HTTP boundary.
pub(crate) struct SafeResyncError {
	pub(crate) message: &'static str,
	pub(crate) status: Status,
}

pub(crate) fn safe_resync_error(error: &ResyncError) -> SafeResyncError {
	// The MESSAGE and the HTTP status stay here: only this surface owes a
	// path-free wording, and only this surface has a status to pick. The CODE
	// comes from the shared mapping on `error.code()`.
	match error {
		ResyncError::Locked(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
			SafeResyncError {
				message: "Another aghub process is mutating skills; retry shortly",
				status: Status::Conflict,
			}
		}
		ResyncError::StaleFetch(_) => SafeResyncError {
			message:
				"The skill's source changed while this sync was fetching; \
			          nothing was written. Re-run to use the current source",
			status: Status::Conflict,
		},
		ResyncError::NotInstalled => SafeResyncError {
			message: "Skill is locked but no installed copy was found",
			status: Status::NotFound,
		},
		// Unreached in production (both callers intercept `Renamed` earlier to
		// build the name-carrying message).
		ResyncError::Renamed { .. } => SafeResyncError {
			message: "Source skill was renamed",
			status: Status::BadRequest,
		},
		ResyncError::Parse(_) => SafeResyncError {
			message: "Failed to parse synced skill",
			status: Status::BadRequest,
		},
		ResyncError::Conflict(_) => SafeResyncError {
			message: "The update was refused because a separate installed copy differs from its Master. Both copies were kept; compare them before consolidating.",
			status: Status::Conflict,
		},
		ResyncError::OutOfTree(_) => SafeResyncError {
			message: "Refusing to sync out-of-tree target",
			status: Status::BadRequest,
		},
		ResyncError::Hash(_)
		| ResyncError::Swap(_)
		| ResyncError::Locked(_) => SafeResyncError {
			message: "Failed to sync skill",
			status: Status::InternalServerError,
		},
		ResyncError::LockUpdate(_) => SafeResyncError {
			message: "Failed to update skill lock after sync",
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
			ResyncError::Locked(std::io::Error::new(
				std::io::ErrorKind::WouldBlock,
				sentinel,
			)),
			ResyncError::StaleFetch(sentinel.to_string()),
			ResyncError::Conflict(sentinel.to_string()),
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
		}
	}
}
