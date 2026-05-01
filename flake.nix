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
        nixpkgs.lib.genAttrs [ "x86_64-linux" ]
          (system: f system (import nixpkgs { inherit system; }));
      vulnix = self.packages.x86_64-linux.vulnix;
    in
    {
      packages = eachSystem (_: pkgs: {
        default = pkgs.callPackage ./package.nix { };
        vulnix = pkgs.vulnix.overrideAttrs (_: { patches = [ ./vulnix-1.12.1-storedir.patch ]; });
      });

      apps = eachSystem (system: _: {
        default = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${system}.default;
          meta.description = self.packages.${system}.default.meta.description;
        };
      });

      devShells = eachSystem (system: pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ rustup cargo clippy rust-analyzer util-linux libvirt virtiofsd openssh mitmproxy vulnix ];
          # Avoid python namespace collisions between mitmproxy and vulnix wrappers.
          shellHook = ''
            unset PYTHONPATH
          '';
          LIBVIRT_DEFAULT_URI = "qemu:///session";
        };
      });

      checks = eachSystem (_: pkgs: {
        nix-parse = pkgs.runCommand "nix-parse" { } ''
          export NIX_STATE_DIR="$TMPDIR/nix-state"
          mkdir -p "$NIX_STATE_DIR/profiles"
          ${pkgs.nix}/bin/nix-instantiate --parse ${./test.nix} >/dev/null
          ${pkgs.nix}/bin/nix-instantiate --parse ${template/flake.nix} >/dev/null
          ${pkgs.nix}/bin/nix-instantiate --parse ${template/agentsandbox/flake.nix} >/dev/null
          touch "$out"
        '';
      });
    };
}
