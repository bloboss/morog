//! `morog` — research toolkit CLI over the Source-Topic-Argument note model.
//!
//! `lint` (M2), `index`, `query` (M4), `research` (M5), and `graph` (M6)
//! are implemented. See `../plan.md` (repository sibling) for the design
//! document.

mod bib;
mod cli;
mod config;
mod graph;
mod index;
mod init;
mod json;
mod lint;
mod lsp;
mod publish;
mod query;
mod research;
mod tags;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { force, vault } => init::run(vault, force),
        Command::Index {
            vault,
            full,
            watch,
            bib,
        } => index::run(vault, full, watch, &cli.format, &bib),
        Command::Query { query, vault } => query::run(&query, vault, &cli.format),
        Command::Research { command } => research::run(command, &cli.format),
        Command::Graph(args) => graph::run(args, &cli.format),
        Command::Tags { name, vault } => tags::run(&name, vault, &cli.format),
        Command::Lint { files, bib } => lint::run(files, &cli.format, &bib),
        Command::Lsp => lsp::run(),
        Command::Publish { command } => publish::run(command, &cli.format),
    }
}
