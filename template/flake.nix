{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager/release-25.11";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
    agentsandbox.url = "path:./agentsandbox";
    agentsandbox.inputs.nixpkgs.follows = "nixpkgs";
    opencode.url = "github:anomalyco/opencode/dev";
    opencode.inputs.nixpkgs.follows = "nixpkgs-unstable";
  };

  outputs = { self, nixpkgs, nixpkgs-unstable, home-manager, agentsandbox, opencode, ... }:
    let
      nixosWithOverlay = system: modules:
        nixpkgs.lib.nixosSystem {
          inherit system;
          modules = modules ++ [
            home-manager.nixosModules.home-manager
            agentsandbox.nixosModules.default
            (_: { nixpkgs.overlays = [ (_: _: self.packages.${system}) ]; })
          ];
          specialArgs.pkgs-unstable = import nixpkgs-unstable { inherit system; config.allowUnfree = true; };
          specialArgs.nixpkgs = nixpkgs;
        };
      eachSystem = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
    in
    {
      packages = eachSystem (system: {
        # Extra packages available.
        opencode-dev = opencode.packages.${system}.opencode.overrideAttrs (old: {
          preBuild = (old.preBuild or "") + ''
            substituteInPlace packages/opencode/src/cli/cmd/generate.ts \
              --replace-fail 'const prettier = await import("prettier")' 'const prettier: any = { format: async (s: string) => s }' \
              --replace-fail 'const babel = await import("prettier/plugins/babel")' 'const babel = {}' \
              --replace-fail 'const estree = await import("prettier/plugins/estree")' 'const estree = {}'
            substituteInPlace package.json \
              --replace-fail '"packageManager": "bun@1.3.13"' '"packageManager": "bun@1.3.11"'
          '';
        });
      });
      nixosConfigurations.default = nixosWithOverlay "x86_64-linux" [ ./configuration.nix ];
    };
}
