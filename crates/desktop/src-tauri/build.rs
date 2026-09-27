fn main() {
	// tauri-build >= 2.7 statically links the VC runtime on every MSVC build by
	// default. Its stub `msvcrt.lib` lands in OUT_DIR, which cargo also hands to
	// other crates' doctests under `cargo test --workspace`, and they then fail
	// to link (unresolved `__CxxFrameHandler3`). Keep the static runtime for
	// release builds (`tauri build`, what ships) and skip it everywhere else.
	let release = std::env::var("PROFILE").is_ok_and(|p| p == "release");
	let windows =
		tauri_build::WindowsAttributes::new().static_vc_runtime(release);
	if let Err(error) = tauri_build::try_build(
		tauri_build::Attributes::new().windows_attributes(windows),
	) {
		panic!("tauri build script failed: {error:#}");
	}
}
