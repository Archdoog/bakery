//! Bakery CLI entry point.

mod cli;
mod commands;
mod config;
mod github;
mod host;
mod models;
mod paths;
mod provision;
mod runner;
mod state;
mod tart;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    match commands::run(cli::Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(1)
        }
    }
}
