//! Plugin ownership, injected ONCE per process by each surface.
//!
//! Core does not depend on the plugin crate. Each surface (API, CLI) installs a
//! provider at startup; every skill mutation entry reads the roots BEFORE it
//! takes the mutation lock, because reading them may spawn `claude`. No
//! provider, or an unreadable plugin list, yields an empty list, which refuses
//! nothing (spec #24 decision 7).

use std::path::PathBuf;
use std::sync::OnceLock;

/// `(plugin name, install root)` for every installed Claude Code plugin.
pub type PluginRootsProvider = fn() -> Vec<(String, PathBuf)>;

static PROVIDER: OnceLock<PluginRootsProvider> = OnceLock::new();

/// Install the surface's provider. The first call wins; later calls are ignored.
pub fn install_provider(provider: PluginRootsProvider) {
	let _ = PROVIDER.set(provider);
}

/// The plugin roots read for ONE mutation. Constructed only through
/// [`PluginRoots::current`] / [`PluginRoots::current_if`].
pub(crate) struct PluginRoots(Vec<(String, PathBuf)>);

impl PluginRoots {
	/// Read the roots now. Call this before taking the mutation lock.
	pub(crate) fn current() -> Self {
		#[cfg(test)]
		{
			if let Some(roots) = test_override::get() {
				return Self(roots);
			}
		}
		Self(match PROVIDER.get() {
			Some(provider) => provider(),
			None => Vec::new(),
		})
	}

	/// A flow that removes nothing cannot touch a plugin's skill, so it reads
	/// nothing and spawns no `claude`.
	pub(crate) fn current_if(needed: bool) -> Self {
		if needed {
			Self::current()
		} else {
			Self(Vec::new())
		}
	}

	pub(crate) fn as_slice(&self) -> &[(String, PathBuf)] {
		&self.0
	}
}

#[cfg(test)]
pub(crate) mod test_override {
	use std::cell::RefCell;
	use std::path::PathBuf;

	thread_local! {
		static ROOTS: RefCell<Option<Vec<(String, PathBuf)>>> = const { RefCell::new(None) };
	}

	pub(super) fn get() -> Option<Vec<(String, PathBuf)>> {
		ROOTS.with(|roots| roots.borrow().clone())
	}

	/// Override the roots on this thread until the guard drops.
	#[must_use]
	pub(crate) fn set(roots: Vec<(String, PathBuf)>) -> Guard {
		ROOTS.with(|slot| *slot.borrow_mut() = Some(roots));
		Guard
	}

	pub(crate) struct Guard;

	impl Drop for Guard {
		fn drop(&mut self) {
			ROOTS.with(|slot| *slot.borrow_mut() = None);
		}
	}
}
