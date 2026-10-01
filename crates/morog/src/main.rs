//! `morog` — research toolkit CLI over the Source-Topic-Argument note model.
//!
//! `lint` (M2), `index`, and `query` (M4) are implemented; the other
//! subcommands are stubs while the toolkit is scaffolded. See `../plan.md`
//! (repository sibling) for the design document.

mod bib;
mod cli;
mod index;
mod json;
mod lint;
mod query;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Index {
            vault,
            full,
            watch,
            bib,
        } => {
            return index::run(vault, full, watch, &cli.format, &bib);
        }
        Command::Query { query, vault } => return query::run(&query, vault, &cli.format),
        Command::Research => "research",
        Command::Graph { .. } => "graph",
        Command::Lint { files, bib } => return lint::run(files, &cli.format, &bib),
    };
    println!("morog {name}: not yet implemented (format: {})", cli.format);
    ExitCode::SUCCESS
}
