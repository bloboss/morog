{
  description = "morog — research toolkit with Source-Topic-Argument notes on morg";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "clippy" "rustfmt" ];
        };

        nativeBuildInputs = with pkgs; [
          rustToolchain
          pkg-config
        ];
      in
      {
        # No package output yet: morog depends on the sibling checkout
        # ../morg/crates/morg-parser via a path dependency, which lies
        # outside this flake's source. A package build lands once
        # morg-parser is consumable as a flake input or from a registry.

        devShells.default = pkgs.mkShell {
          inherit nativeBuildInputs;
          packages = with pkgs; [
            cargo-watch
            graphviz # render `morog graph` DOT output
          ];

          shellHook = ''
            echo "morog dev shell"
            echo "  cargo build --workspace   — build all crates"
            echo "  cargo test --workspace    — run tests"
            echo "  cargo run -p morog -- …   — run the CLI"
          '';
        };
      });
}
