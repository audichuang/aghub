use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use aghub_api::cli::{
	capabilities_string, parse_args, version_string, PORT_LINE_PREFIX,
};
use aghub_api::{start_with_port_reporter, ApiOptions};

#[tokio::main]
async fn main() -> ExitCode {
	let config = match parse_args(std::env::args().collect()) {
		Ok(config) => config,
		Err(error) => {
			eprintln!("{error}");
			return ExitCode::FAILURE;
		}
	};

	if config.version {
		println!("{}", version_string());
		return ExitCode::SUCCESS;
	}

	// Advertise wire capabilities WITHOUT starting the server, so the desktop
	// remote bring-up can detect feature support over SSH. An old binary
	// predating this flag returns a parse error above, which the probe treats
	// as "capability unsupported".
	if config.capabilities {
		println!("{}", capabilities_string());
		return ExitCode::SUCCESS;
	}

	// Emit the port line only AFTER Rocket has bound its listener, so an SSH
	// caller polling the log learns the real port (correct even for an
	// ephemeral `--port 0`) and never races a bind that may still fail.
	let reporter = Arc::new(|port: u16| {
		println!("{PORT_LINE_PREFIX}{port}");
		let _ = std::io::stdout().flush();
	});

	match start_with_port_reporter(
		ApiOptions {
			// Pass the requested port straight through; `0` means let the OS
			// assign an ephemeral port at bind time.
			port: config.port,
			app_data_dir: None,
		},
		Some(reporter),
	)
	.await
	{
		Ok(()) => ExitCode::SUCCESS,
		Err(error) => {
			eprintln!("server error: {error}");
			ExitCode::FAILURE
		}
	}
}
