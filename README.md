# morog

Research toolkit implementing a **Source-Topic-Argument** (STA) note model on
top of [morg](../morg). Notes stay plain markdown in a vault; morog derives a
typed discourse graph from them through a disposable SQLite index, with
bibliography resolution, lints, DOT graph output, and editor tooling.

Design document: [`../plan.md`](../plan.md). Everything here is scaffolding;
no features are implemented yet.

## Crate map

| Crate | Kind | Role |
|---|---|---|
| [`crates/morog`](crates/morog) | bin | CLI: `index`, `query`, `research`, `graph`, `lint` (stubs) |
| [`crates/morog-sema`](crates/morog-sema) | lib | Typed STA frontmatter, `FileFacts` extraction, lints |
| [`crates/morog-bib`](crates/morog-bib) | lib | `BibStore` trait over citation keys; bibrat adapter pending, `FixtureStore` for now |
| [`crates/morog-index`](crates/morog-index) | lib | Disposable SQLite cache under `$XDG_CACHE_HOME/morog/<vault-hash>/` |
| [`crates/morog-lsp`](crates/morog-lsp) | lib | Language server, rust-analyzer-style (main loop, vfs, analysis snapshots) |

Parsing is provided by the sibling checkout's
[`morg-parser`](../morg/crates/morg-parser) as a path dependency, so a `morg`
checkout must sit next to this repository.

## Development

```sh
cargo build --workspace
cargo test --workspace
cargo run -p morog -- lint notes/
```

A nix dev shell is available via `nix develop`, and pre-commit hooks mirror
CI (`pre-commit install`).
