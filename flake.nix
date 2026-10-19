{
  description = "shell env";

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
  in
  {
    devShells.${system}.default = pkgs.mkShell {
        name = "env";
        buildInputs = [
          codex-cli-nix.packages.${system}.default
          (fenix.packages.${system}.stable.withComponents [ "cargo" "rustc" "rustfmt" "clippy" ])
          pkgs.neovim
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
