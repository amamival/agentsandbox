{
  description = "A secure, efficient, reproducible NixOS Linux VM for self-improving agentic workflows";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    /* the following inputs are used in test.nix */
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager/release-25.11";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
    impermanence.url = "github:nix-community/impermanence";
    impermanence.inputs.nixpkgs.follows = "nixpkgs";
    impermanence.inputs.home-manager.follows = "home-manager";
  };
  outputs = { self, nixpkgs, ... }:
    let
      eachSystem = f:
        nixpkgs.lib.genAttrs [ "aarch64-linux" "x86_64-linux" ]
          (system: f system (import nixpkgs { inherit system; }));
    in
    {
      packages = eachSystem (_: pkgs: {
        default = pkgs.callPackage ./package.nix { };
      });

      apps = eachSystem (system: _: {
        default = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${system}.default;
          meta.description = self.packages.${system}.default.meta.description;
        };
      });

      devShells = eachSystem (_: pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            bubblewrap cargo clippy curl jq libvirt mitmproxy openssh passt
            rust-analyzer rustc rustfmt socat util-linux virtiofsd zstd
          ];
          LIBVIRT_DEFAULT_URI = "qemu:///session";
        };
      });

      checks = eachSystem (_: pkgs: {
        nix-parse = pkgs.runCommand "nix-parse" { } ''
          export NIX_STATE_DIR="$TMPDIR/nix-state"
          mkdir -p "$NIX_STATE_DIR/profiles"
          ${pkgs.nix}/bin/nix-instantiate --parse ${./test.nix} >/dev/null
          ${pkgs.nix}/bin/nix-instantiate --parse ${./share/agentsandbox/template/flake.nix} >/dev/null
          ${pkgs.nix}/bin/nix-instantiate --parse ${./share/agentsandbox/template/agentsandbox/flake.nix} >/dev/null
          touch "$out"
        '';
      });
    };
}
