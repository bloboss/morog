//! `morog` — research toolkit CLI over the Source-Topic-Argument note model.
//!
//! All subcommands are stubs while the toolkit is scaffolded; see
//! `../plan.md` (repository sibling) for the design document.

mod cli;

use clap::Parser;

use cli::{Cli, Command};

fn main() {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Index { .. } => "index",
        Command::Query { .. } => "query",
        Command::Research => "research",
        Command::Graph { .. } => "graph",
        Command::Lint { .. } => "lint",
    };
    println!("morog {name}: not yet implemented (format: {})", cli.format);
}
