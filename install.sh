#!/bin/sh
# Install the morog research toolkit from this checkout.
#
# Builds and installs the `morog` binary (CLI + LSP server in one) to
# ~/.cargo/bin via `cargo install`. Requires a Rust toolchain; network
# access is needed on first build to fetch the pinned morg-parser git
# dependency.
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found; install Rust via rustup first" >&2
    exit 1
fi

echo "Installing morog from $repo_dir ..."
cargo install --locked --path "$repo_dir/crates/morog"

bin_dir=${CARGO_HOME:-$HOME/.cargo}/bin
echo
echo "Installed: $bin_dir/morog"
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) echo "note: $bin_dir is not on your PATH" ;;
esac

# Write the starter user config; an existing one is left untouched, so the
# script stays idempotent.
echo
"$bin_dir/morog" init

cat <<'EOF'

Next steps:
  morog index          # build the disposable index for the current vault
  morog lint           # run the STA lints
  morog lsp            # stdio LSP server (wire into your editor)
  morog init --vault   # vault-local .morog.toml overrides
EOF
