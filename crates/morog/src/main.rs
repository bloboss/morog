//! `morog` — research toolkit CLI over the Source-Topic-Argument note model.
//!
//! `lint` (M2), `index`, `query` (M4), `research` (M5), and `graph` (M6)
//! are implemented. See `../plan.md` (repository sibling) for the design
//! document.

mod bib;
mod cli;
mod graph;
mod index;
mod json;
mod lint;
mod query;
mod research;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Index {
            vault,
            full,
            watch,
            bib,
        } => index::run(vault, full, watch, &cli.format, &bib),
        Command::Query { query, vault } => query::run(&query, vault, &cli.format),
        Command::Research { command } => research::run(command, &cli.format),
        Command::Graph(args) => graph::run(args, &cli.format),
        Command::Lint { files, bib } => lint::run(files, &cli.format, &bib),
    }
}
