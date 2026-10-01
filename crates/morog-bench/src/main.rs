//! M0 benchmark tool: generate a ~5M-word synthetic STA vault from a text
//! corpus, and time the parse / index / cold-load pipeline against the
//! plan.md section 5.7 performance budgets.

mod bench;
mod corpus;
mod generate;
mod rng;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "morog-bench",
    about = "Synthetic vault generator and timing harness (milestone M0)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a synthetic STA vault from a plain-text corpus
    Gen {
        /// Directory of corpus .txt files
        #[arg(long, default_value = "bench/corpus")]
        corpus: PathBuf,
        /// Output vault directory (wiped and recreated)
        #[arg(long, default_value = "bench/vault")]
        out: PathBuf,
        /// Target total word count
        #[arg(long, default_value_t = 5_000_000)]
        words: u64,
        /// PRNG seed (the vault is deterministic for a seed)
        #[arg(long, default_value_t = 0x5EED_0A57)]
        seed: u64,
    },
    /// Time parse, index, and cold-load passes over a generated vault
    Bench {
        /// Vault directory to benchmark
        #[arg(long, default_value = "bench/vault")]
        vault: PathBuf,
        /// Throwaway SQLite database path
        #[arg(long, default_value = "bench/bench-index.sqlite")]
        db: PathBuf,
        /// Files per insert transaction (plan 3.2 uses 256)
        #[arg(long, default_value_t = 256)]
        batch: usize,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Gen {
            corpus,
            out,
            words,
            seed,
        } => generate::run(&corpus, &out, words, seed).map(|_| ()),
        Command::Bench { vault, db, batch } => bench::run(&vault, &db, batch.max(1)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("morog-bench: {e}");
            ExitCode::FAILURE
        }
    }
}
