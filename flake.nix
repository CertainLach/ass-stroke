{
  description = "Diagnostics library";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };
  };
  outputs = { nixpkgs, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        rust = ((pkgs.rustChannelOf { date = "2022-12-13"; channel = "nightly"; }).default.override {
          extensions = [ "rust-src" "miri" ];
        });
      in
      rec {
        devShell = pkgs.mkShell {
          nativeBuildInputs = with pkgs;[
            rust
            cargo-edit
            lld
            asciinema
          ];
        };
      }
    );
}
