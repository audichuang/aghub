use std::io::Write;
use std::process::ExitCode;

use aghub_api::cli::{
	parse_args, pick_free_port, version_string, Config, PORT_LINE_PREFIX,
};
use aghub_api::{start, ApiOptions};

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

	let port = match resolve_port(&config) {
		Ok(port) => port,
		Err(error) => {
			eprintln!("failed to pick a free port: {error}");
			return ExitCode::FAILURE;
		}
	};

	// Emit the port line and flush so an SSH caller reading the log can parse
	// it before the (potentially long-lived) server starts.
	println!("{PORT_LINE_PREFIX}{port}");
	let _ = std::io::stdout().flush();

	match start(ApiOptions {
		port,
		app_data_dir: None,
	})
	.await
	{
		Ok(()) => ExitCode::SUCCESS,
		Err(error) => {
			eprintln!("server error: {error}");
			ExitCode::FAILURE
		}
	}
}

fn resolve_port(config: &Config) -> std::io::Result<u16> {
	if config.port == 0 {
		pick_free_port()
	} else {
		Ok(config.port)
	}
}
