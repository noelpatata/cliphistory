//! cliphistory — modular clipboard history manager for Linux.
//!
//! All logic lives in the `cliphistory-core` library; this binary only wires up
//! argument parsing and logging.

use clap::Parser;
use cliphistory_core::cli::{self, Cli};
use cliphistory_core::config;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    let cfg = match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            return std::process::ExitCode::FAILURE;
        }
    };

    init_logging(&cfg);

    match cli::execute(cli.command, &cfg) {
        Ok(code) => exit_code(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn exit_code(code: i32) -> std::process::ExitCode {
    std::process::ExitCode::from(code.clamp(0, 255) as u8)
}

fn init_logging(cfg: &config::Config) {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| cfg.general.log_level.clone());
    // Propagate the effective level to every spawned module (they inherit
    // our environment) so module-side logs are tunable from this one knob.
    std::env::set_var("RUST_LOG", &filter);
    env_logger::Builder::new()
        .filter_level(filter.parse().unwrap_or(log::LevelFilter::Info))
        .format_timestamp_secs()
        .init();
}
