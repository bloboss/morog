//! `morog` — research toolkit CLI over the Source-Topic-Argument note model.
//!
//! `lint` is implemented (milestone M2); the other subcommands are stubs
//! while the toolkit is scaffolded. See `../plan.md` (repository sibling)
//! for the design document.

mod cli;
mod lint;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Index { .. } => "index",
        Command::Query { .. } => "query",
        Command::Research => "research",
        Command::Graph { .. } => "graph",
        Command::Lint { files } => return lint::run(files, &cli.format),
    };
    println!("morog {name}: not yet implemented (format: {})", cli.format);
    ExitCode::SUCCESS
}
