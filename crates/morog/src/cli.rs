use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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
        /// Rebuild from scratch into a temp file, then swap it in atomically
        #[arg(long)]
        full: bool,
        /// Keep running and re-index on file changes
        #[arg(long)]
        watch: bool,
        #[command(flatten)]
        bib: BibArgs,
    },
    /// Query the index (notes, relations, citations)
    Query {
        /// A single SELECT statement; the v_* views are the public API
        query: String,
        /// Vault root directory the index belongs to (default: current directory)
        #[arg(long)]
        vault: Option<PathBuf>,
    },
    /// Research workflows over the STA graph (create, list, check notes)
    Research {
        #[command(subcommand)]
        command: ResearchCommand,
    },
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
        #[command(flatten)]
        bib: BibArgs,
    },
}

/// The `morog research` subcommands (plan §7 M5): note creation from the
/// typed STA schemas, a vault listing, and an STA-restricted lint pass.
#[derive(Subcommand)]
pub enum ResearchCommand {
    /// Create a source note for a bibliography key (the id IS the key)
    Source {
        /// Bibrat citation key; becomes the note id and file name
        key: String,
        /// Vault root directory to create the note in (default: current directory)
        #[arg(long, value_name = "DIR")]
        vault: Option<PathBuf>,
        #[command(flatten)]
        bib: BibArgs,
    },
    /// Create a topic note (id slugified from TITLE)
    Topic {
        /// Note title
        title: String,
        /// Seed the body with one evidence block citing KEY (repeatable)
        #[arg(long = "cite", value_name = "KEY")]
        cites: Vec<String>,
        /// Vault root directory to create the note in (default: current directory)
        #[arg(long, value_name = "DIR")]
        vault: Option<PathBuf>,
    },
    /// Create an argument note (id slugified from TITLE)
    Argument {
        /// Note title
        title: String,
        /// Declare a relation to ID (optionally ID#anchor), e.g.
        /// `--to specific-leaf-area:supports:analyze` (repeatable)
        #[arg(long = "to", value_name = "ID:STANCE[:OP]")]
        to: Vec<String>,
        /// Vault root directory to create the note in (default: current directory)
        #[arg(long, value_name = "DIR")]
        vault: Option<PathBuf>,
    },
    /// Tabulate the vault's STA notes by kind, status, and title
    List {
        /// Vault root directory (default: current directory)
        vault: Option<PathBuf>,
    },
    /// Lint the vault restricted to STA notes (alias for `morog lint`)
    Check {
        /// Markdown files or directories to check
        files: Vec<PathBuf>,
        #[command(flatten)]
        bib: BibArgs,
    },
}

/// How a subcommand finds the bibrat store (plan §4). The default is
/// auto-discovery: config `[store] data_dir`, else `$BIBRAT_DATA_DIR`,
/// else `$XDG_DATA_HOME/bibrat`, else `~/.local/share/bibrat` — missing
/// store means the bibliography-backed behavior is simply skipped.
#[derive(Debug, Args)]
#[group(multiple = false)]
pub struct BibArgs {
    /// Require the bibrat store (default is auto-discovery, where a
    /// missing store is skipped; this makes it an error)
    #[arg(long)]
    pub bib: bool,

    /// Run without a bibliography (bib-backed lints skipped, bib_entries
    /// left empty)
    #[arg(long)]
    pub no_bib: bool,

    /// Bibrat data directory to use instead of auto-discovery
    #[arg(long, value_name = "DIR")]
    pub bib_root: Option<PathBuf>,
}
