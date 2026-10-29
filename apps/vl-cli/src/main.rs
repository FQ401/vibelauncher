//! `vl-cli`: the launcher without a window, over the same facade as the desktop UI.
//!
//! ```text
//! vl-cli [--mock] [--dir <path>] <command>
//! ```
//! `--mock` runs on synthetic data (`vl-mock`); otherwise the real backend (`vl-backend`) serves
//! the commands. `--dir` selects the data folder (as does the `VIBELAUNCHER_DIR` variable);
//! otherwise the portable marker, the pointer file or the OS convention decides.

mod commands;
mod progress;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use vl_backend::{Backend, BackendConfig};
use vl_core::Facade;
use vl_mock::{MockFacade, MockOptions};

fn main() -> ExitCode {
    if let Some(code) = vl_backend::run_helper_if_requested() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let result =
        start(&args).and_then(|(facade, rest)| commands::run(facade.as_ref(), rest, &mut out));
    let _ = out.flush();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "vl-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Reads the global options and starts the facade; returns it with the remaining arguments.
fn start<'a>(
    mut args: &'a [&'a str],
) -> Result<(Box<dyn Facade>, &'a [&'a str]), commands::CliError> {
    let mut mock = false;
    let mut config = BackendConfig::default();
    loop {
        match args {
            ["--mock", rest @ ..] => {
                mock = true;
                args = rest;
            }
            ["--dir", dir, rest @ ..] => {
                config.data_dir = Some(PathBuf::from(dir));
                args = rest;
            }
            _ => break,
        }
    }
    let facade: Box<dyn Facade> = if mock {
        Box::new(MockFacade::new(MockOptions {
            step_delay: Duration::from_millis(30),
            game_log_lines: 20,
            exit_after_log: true,
            game_crashes: false,
            curseforge_key: false,
        }))
    } else {
        Box::new(Backend::new(config)?)
    };
    Ok((facade, args))
}
