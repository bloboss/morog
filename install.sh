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

cat <<'EOF'

Next steps:
  morog index          # build the disposable index for the current vault
  morog lint           # run the STA lints
  morog lsp            # stdio LSP server (wire into your editor)

Configuration (merged key-by-key, later wins):
  ~/.config/morog/config.toml     user config ([publish], [lint], [tags])
  <vault>/.morog.toml             vault-local overrides
  [tags] also falls back to ~/.config/morg/config.toml
EOF
