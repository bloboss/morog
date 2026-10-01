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
//!
//! The main loop, protocol handlers, diagnostics publishing, and the
//! embedded indexer build on these in the following layers of milestone M7.

pub mod analysis;
pub mod vfs;
