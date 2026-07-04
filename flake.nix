{
  description = "Development shell for the Electrode web ground station";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            clippy
            git
            nodejs_24
            pkg-config
            rust-analyzer
            rustc
            rustfmt
            wasm-pack
          ];

          buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.udev
          ];

          shellHook = ''
            export ELECTRODE_DEV_SHELL=1
            echo "electrode-web dev shell"
            echo "  npm ci"
            echo "  npm run ci"
            echo "  cargo test --workspace --all-targets"
          '';
        };
      }
    );
}
