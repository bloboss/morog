//! Language server for morog vaults, in rust-analyzer style (plan §5).
//!
//! Layering:
//!
//! - [`vfs`] — `FileId` interning and open-document overlays; open buffers
//!   always win over disk and index.
//! - [`analysis`] — the immutable, cheaply cloneable snapshot: per-file
//!   [`analysis::FileAnalysis`], the cross-file maps ([`analysis::Xref`]),
//!   the bibliography table, and cursor classification. Writes happen on the
//!   main thread by structural sharing; readers clone and run on workers.
//! - [`config`], [`to_proto`], [`from_proto`] — the protocol boundary:
//!   `initializationOptions`, and position conversion in the negotiated
//!   encoding (UTF-8 preferred, UTF-16 fallback — plan §5.4).
//! - [`state`], [`diagnostics`], [`main_loop`] — the single-threaded event
//!   loop over `lsp-server`, document sync (plan §5.3), and debounced
//!   `publishDiagnostics` with the full `sta::*` set (milestone M7 L1).
//!
//! The embedded indexer and the L2 navigation handlers build on these in
//! the remaining layers of milestone M7.

pub mod analysis;
pub mod config;
mod diagnostics;
mod from_proto;
mod handlers;
mod indexer;
pub mod main_loop;
mod state;
mod to_proto;
pub mod vfs;

pub use main_loop::{run, run_stdio};

/// The negotiated position encoding (plan §5.4): spans are UTF-8 bytes
/// internally; UTF-16 exists only at the protocol boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

/// Boxed error for the server entry points.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
