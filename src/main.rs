mod app;
mod backends;
mod cli;
mod history;
mod host;
mod models;
mod navigator;
mod popup;
mod theme;
mod ui;

#[cfg(test)]
mod test_helpers;

use anyhow::Result;
use clap::Parser;
use std::process::ExitCode;

fn main() -> Result<ExitCode> {
    let cli = cli::Cli::parse();
    match cli.command {
        Some(cli::Command::Rex(command)) => backends::rex::run(command, cli.view),
        _ => backends::herdr::run(cli).map(|()| ExitCode::SUCCESS),
    }
}
