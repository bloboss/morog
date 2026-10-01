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
    /// Emit the discourse graph as DOT (plan §7.1)
    Graph(GraphArgs),
    /// Lint STA notes (frontmatter, provenance, relations)
    Lint {
        /// Markdown files or directories to lint
        files: Vec<PathBuf>,
        #[command(flatten)]
        bib: BibArgs,
    },
    /// Run the language server over stdio (plan §5)
    ///
    /// The vault root and bibrat root come from the client's
    /// initializationOptions (`vaultRoot`, `bibratRoot`), falling back to
    /// the first workspace folder and bibrat auto-discovery.
    Lsp,
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

/// Flags of `morog graph` (plan §7.1): neighborhood restriction, node and
/// edge filters, the evidence (discourse-graph) expansion, and output. The
/// global `--format` selects DOT (`text`, the default) or a plain
/// node/edge-list JSON.
#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Restrict the graph to the neighborhood of this note id
    #[arg(long, value_name = "ID")]
    pub root: Option<String>,

    /// Neighborhood radius in hops, following edges in both directions
    /// (default 1; requires --root)
    #[arg(long, value_name = "N", requires = "root")]
    pub depth: Option<usize>,

    /// Keep only these note kinds (comma-separated: source,topic,argument)
    #[arg(long, value_name = "KIND,...", value_delimiter = ',')]
    pub kinds: Vec<String>,

    /// Keep only edges with these stances (comma-separated:
    /// supports,opposes,qualifies); unstanced edges are dropped
    #[arg(long, value_name = "STANCE,...", value_delimiter = ',')]
    pub stance: Vec<String>,

    /// Keep only edges whose Bloom op is at least this deep
    /// (recall..create); edges without an op are dropped
    #[arg(long, value_name = "OP")]
    pub min_op: Option<String>,

    /// Expand each topic into a cluster of its evidence blocks
    /// (the discourse-graph view)
    #[arg(long)]
    pub evidence: bool,

    /// Keep only notes with `publish: true`
    #[arg(long)]
    pub published: bool,

    /// Output file (default: stdout)
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Vault root directory the index belongs to (default: current directory)
    #[arg(long, value_name = "DIR")]
    pub vault: Option<PathBuf>,
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
