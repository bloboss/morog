//! Language server for morog vaults, in rust-analyzer style.
//!
//! Planned layout (nothing implemented yet, and no LSP dependencies pulled
//! in until the skeleton is real):
//!
//! - **`main_loop`** — the server's single-threaded event loop: multiplexes
//!   LSP requests/notifications, vfs change events, and finished background
//!   tasks; owns the global state and hands out snapshots.
//! - **`vfs`** — an in-memory view of vault files, layered over the disk:
//!   open editor buffers shadow on-disk content, changes arrive as events
//!   with document versions.
//! - **analysis snapshots** — immutable, cheaply cloneable views of the
//!   analysis state (parsed documents, `FileFacts`, the citation table).
//!   Requests run against a snapshot on worker threads and are cancelled by
//!   simply dropping the snapshot when newer state exists, so typing never
//!   blocks on long analysis.
//!
//! Features this will serve: completion for ids and citation keys,
//! go-to-definition across notes, stance-aware backlinks, and inline lint
//! diagnostics from `morog-sema`.
