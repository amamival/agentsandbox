{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager/release-25.11";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
  };
  outputs = { self, nixpkgs, nixpkgs-unstable, home-manager }:
    let
      nixosWithOverlay = system: modules:
        nixpkgs.lib.nixosSystem {
          inherit system;
          modules = modules ++ [
            home-manager.nixosModules.home-manager
            (_: { nixpkgs.overlays = [ (_: _: self.packages.${system}) ]; })
          ];
          specialArgs.pkgs-unstable = import nixpkgs-unstable { inherit system; config.allowUnfree = true; };
        };
      eachSystem = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
    in
    {
      packages = eachSystem (system: rec {
        _pkgs = import nixpkgs { inherit system; config.allowUnfree = true; };
        # Extra packages available.
      });
      nixosConfigurations.agenthouse = nixosWithOverlay "x86_64-linux" [ ./configuration.nix ];
    };
}

