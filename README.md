# morog

Research toolkit implementing a **Source-Topic-Argument** (STA) note model on
top of [morg](../morg). Notes stay plain markdown in a vault; morog derives a
typed discourse graph from them through a disposable SQLite index, with
bibliography resolution, lints, DOT graph output, a language server, and a
publishing pipeline (static site, RDF, micropublications).

Design document: [`../plan.md`](../plan.md).

## Crate map

| Crate | Kind | Role |
|---|---|---|
| [`crates/morog`](crates/morog) | bin | CLI: `index`, `query`, `lint`, `research`, `graph`, `lsp`, `publish` |
| [`crates/morog-sema`](crates/morog-sema) | lib | Typed STA frontmatter, `FileFacts` extraction, the `sta::*` / `pub::leak` lints |
| [`crates/morog-bib`](crates/morog-bib) | lib | `BibStore` trait over citation keys; read-only bibrat adapter + `FixtureStore` |
| [`crates/morog-index`](crates/morog-index) | lib | Disposable SQLite cache: schema, indexing pipeline, read-only query API |
| [`crates/morog-lsp`](crates/morog-lsp) | lib | Language server, rust-analyzer-style: diagnostics, navigation, completion, rename, code actions |
| [`crates/morog-bench`](crates/morog-bench) | bin | Synthetic-vault generator and timing harness (plan M0) |

Parsing is provided by [`morg-parser`](../morg/crates/morg-parser) as a git
dependency pinned by revision in the workspace `Cargo.toml`; see the comment
there for the `[patch]` stanza that redirects it to a sibling checkout during
local development.

## Commands

Every command takes `--format text|json` (global) and defaults the vault to
the current directory.

| Command | What it does |
|---|---|
| `morog index [--full] [--watch]` | Build or refresh the index; `--watch` re-indexes on file changes |
| `morog query "SELECT …"` | Read-only SQL over the index; the `v_*` views are the public API |
| `morog lint [PATHS] [--bib]` | The `sta::*` and `pub::leak` lints; `--bib` requires a bibrat store |
| `morog research source KEY` | Create a source note for a bibliography key (the id **is** the key) |
| `morog research topic TITLE [--cite KEY]` | Create a topic note, optionally seeded with evidence blocks |
| `morog research argument TITLE --to ID:STANCE[:OP]` | Create an argument note with typed relations |
| `morog research list` / `check` | Tabulate STA notes; lint restricted to STA notes |
| `morog graph [--root ID --depth N] [--evidence] [--published]` | DOT export of the discourse graph; `--evidence` expands topics into evidence clusters |
| `morog lsp` | The language server over stdio |
| `morog completions SHELL` | Shell completion script (zsh, bash, fish, ...) to stdout |
| `morog publish site [-o DIR]` | Static site: one page per `publish: true` note, backlinks, JSON-LD |
| `morog publish rdf [-o FILE]` | Turtle dump of the published subgraph (stance/op, CiTO, PROV-O) |
| `morog publish micropub [-o DIR]` | One JSON-LD micropublication bundle per published argument |

Examples:

```sh
morog index --watch                     # keep the index fresh while editing
morog query "SELECT * FROM v_orphans"   # sources never cited, topics never argued
morog lint --bib notes/                 # includes citekey resolution against bibrat
morog research source wright_leaf_2004
morog research topic "Specific leaf area" --cite wright_leaf_2004
morog research argument "Allocation drives the spectrum" \
    --to specific-leaf-area:supports:analyze --to leaf-mass-ratio:qualifies
morog graph --evidence -o graph.dot && dot -Tsvg graph.dot -o graph.svg
morog publish site -o _site             # needs [publish] base_iri, see below
morog publish rdf | head
```

## The disposable index

The index is a **cache, never a source of truth**. It lives outside the vault
at `$XDG_CACHE_HOME/morog/<vault-hash>/index.sqlite` (default
`~/.cache/morog/…`), is built from read-only access to the vault, and can be
deleted at any time — `morog index` rebuilds it, and a schema-version bump
rebuilds it automatically. Commands that read the index (`query`, `graph`,
`publish`) require an existing one and never build it as a side effect.

## Configuration

morog reads `$XDG_CONFIG_HOME/morog/config.toml` (default
`~/.config/morog/config.toml`), overridden **key by key** by a vault-local
`.morog.toml` at the vault root:

```toml
[publish]
base_iri = "https://notes.example.com"  # where the published site lives;
                                        # note IRIs are minted as <base>/n/<uuid>
site_dir = "theme"                      # optional: template-override directory,
                                        # relative to this file

[lint]
topic_max_words = 500                   # sta::topic-too-long threshold
```

`morog publish` refuses to run without `base_iri`. Only notes with
`publish: true` frontmatter are exported; the `pub::leak` lint warns when a
published note targets a private one, and the exporters render such targets
as unlinked text. The site's default theme is a set of embedded MiniJinja
snippets (`base.html`, `note.html`, `backlinks.html`, `index.html`,
`alias.html`, `style.css`); drop a same-named file into `site_dir` to
override any of them, eleventy-style. Note frontmatter — including the
user-owned `extra:` map — is exposed to templates.

## Shell completions

`morog completions <shell>` writes the completion script for zsh, bash,
fish (and the other shells clap_complete supports) to stdout. It is raw
script output — the global `--format` flag does not apply. For zsh, drop
it as `_morog` into a directory on `fpath` before `compinit` runs:

```sh
morog completions zsh > ~/.local/share/zsh/site-functions/_morog
# in ~/.zshrc, before compinit:
fpath=(~/.local/share/zsh/site-functions $fpath)
```

`install.sh` does this for you when the XDG zsh site-functions directory
is writable. The file is derived output, so regenerate it whenever the
tool changes; with chezmoi, a `run_onchange_` script keyed on the pinned
revs re-runs exactly when they move:

```sh
# run_onchange_completions.sh — morg 1b91bb4 / morog f9c2b51
morog completions zsh > ~/.local/share/zsh/site-functions/_morog
morg completions zsh > ~/.local/share/zsh/site-functions/_morg
```

Completion of dynamic values (citekeys, tag names) is future work — the
index could feed it.

## Installing

`install.sh` builds the `morog` binary into `~/.cargo/bin` and drops the zsh
completion into the XDG user directory (see above). On Arch,
`packaging/PKGBUILD` builds a `morog-git` package instead; it deliberately
installs the binary and the zsh/bash/fish completions system-wide under
`/usr/share`, so the two paths do not overlap. The PKGBUILD sources the
intended GitHub remote, which is not published yet — until it is, switch to
the commented `git+file://` source line and run `cd packaging && makepkg -si`
against this checkout. Either way the build fetches the pinned morg-parser
git dependency over the network during `prepare()`.

## Development

```sh
cargo build --workspace
cargo test --workspace
cargo run -p morog -- lint crates/morog/tests/fixtures/vault
cargo run --release -p morog-bench -- gen   # the 5M-word benchmark vault
```

A nix dev shell is available via `nix develop`, and pre-commit hooks mirror
CI (`pre-commit install`).
