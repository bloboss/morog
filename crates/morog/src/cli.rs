use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "morog",
    about = "Research toolkit — Source-Topic-Argument notes on morg"
)]
pub struct Cli {
    /// Output format: text (default) or json
    #[arg(long, global = true, default_value = "text")]
    pub format: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Build or refresh the disposable SQLite index for a vault
    Index {
        /// Vault root directory (default: current directory)
        vault: Option<PathBuf>,
    },
    /// Query the index (notes, relations, citations)
    Query {
        /// Query expression
        query: String,
    },
    /// Research workflows over the STA graph (open questions, orphans, coverage)
    Research,
    /// Emit the discourse graph as DOT
    Graph {
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Lint STA notes (frontmatter, provenance, relations)
    Lint {
        /// Markdown files or directories to lint
        files: Vec<PathBuf>,
    },
}
