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

# Install the zsh completion into the XDG zsh site-functions directory.
# The file is derived output ("morog completions zsh"), so regenerating it
# on every run keeps the script idempotent.
zsh_fn_dir=${XDG_DATA_HOME:-$HOME/.local/share}/zsh/site-functions
echo
if mkdir -p "$zsh_fn_dir" 2>/dev/null \
    && "$bin_dir/morog" completions zsh > "$zsh_fn_dir/_morog"; then
    echo "Installed zsh completion: $zsh_fn_dir/_morog"
    echo "note: the directory must be on fpath before compinit, e.g. in ~/.zshrc:"
    echo "  fpath=($zsh_fn_dir \$fpath)"
else
    echo "note: could not write $zsh_fn_dir/_morog; generate it manually:"
    echo "  morog completions zsh > /path/on/fpath/_morog"
fi

cat <<'EOF'

Next steps:
  morog index          # build the disposable index for the current vault
  morog lint           # run the STA lints
  morog lsp            # stdio LSP server (wire into your editor)
  morog init --vault   # vault-local .morog.toml overrides
EOF
