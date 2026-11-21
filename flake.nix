{
  description = "buf-review CLI and Neovim plugin";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";
    codex-cli-nix.url = "github:sadjow/codex-cli-nix";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { flake-utils, self, nixpkgs, codex-cli-nix, fenix, ... }:
  let
    system = flake-utils.lib.system.x86_64-linux;
    pkgs = nixpkgs.legacyPackages.${system};
    version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
    cli = pkgs.pkgsStatic.rustPlatform.buildRustPackage {
      pname = "buf-review";
      inherit version;
      src = pkgs.lib.fileset.toSource {
        root = ./.;
        fileset = pkgs.lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./src
          ./tests
          ./lua
          ./plugin
        ];
      };
      cargoLock.lockFile = ./Cargo.lock;
      cargoBuildFlags = [ "--bin" "pr" ];
      nativeCheckInputs = [ pkgs.git pkgs.python3 pkgs.neovim ];
      postPatch = ''
        substituteInPlace tests/pr_intents.rs tests/workspace_cli.rs tests/branch_pull_requests.rs \
          --replace-fail '#!/usr/bin/env python3' '#!${pkgs.python3}/bin/python3'
        patchShebangs tests
      '';
      meta.mainProgram = "pr";
    };
    plugin = pkgs.vimUtils.buildVimPlugin {
      pname = "buf-review";
      inherit version;
      src = pkgs.lib.fileset.toSource {
        root = ./.;
        fileset = pkgs.lib.fileset.unions [ ./lua ./plugin ./doc ];
      };
      postInstall = ''
        substituteInPlace "$target/lua/buf_review/client.lua" \
          --replace-fail "executable = 'pr'" "executable = '${cli}/bin/pr'"
      '';
    };
  in
  {
    packages.${system} = { inherit cli plugin; };
    devShells.${system}.default = pkgs.mkShell {
        name = "env";
        buildInputs = [
          codex-cli-nix.packages.${system}.default
          (fenix.packages.${system}.stable.withComponents [ "cargo" "rustc" "rustfmt" "clippy" ])
          pkgs.python3
          (pkgs.writeShellScriptBin "nvimreview" ''
            (cd "$PROJECTPATH" && cargo build --release --locked --bin pr) || exit $?
            export PATH="$PROJECTPATH/target/release:$PATH"
            exec nvim --cmd 'lua vim.opt.runtimepath:prepend(vim.env.PROJECTPATH)' "$@"
          '')
        ];
        runScript = "zsh";
        shellHook = ''
          export SHELL='zsh'
          export PROJECTPATH="$PWD"
        '';
    };
  };
}
