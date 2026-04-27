{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager/release-25.11";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
    impermanence.url = "github:nix-community/impermanence";
    impermanence.inputs.nixpkgs.follows = "";
    impermanence.inputs.home-manager.follows = "";
    opencode.url = "github:anomalyco/opencode/dev";
    opencode.inputs.nixpkgs.follows = "nixpkgs-unstable";
  };
  outputs = { self, nixpkgs, nixpkgs-unstable, home-manager, impermanence, opencode }:
    let
      nixosWithOverlay = system: modules:
        nixpkgs.lib.nixosSystem {
          inherit system;
          modules = modules ++ [
            home-manager.nixosModules.home-manager
            impermanence.nixosModules.impermanence
            (_: { nixpkgs.overlays = [ (_: _: self.packages.${system}) ]; })
          ];
          specialArgs.pkgs-unstable = import nixpkgs-unstable { inherit system; config.allowUnfree = true; };
        };
      eachSystem = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
    in
    {
      packages = eachSystem
        (system:
          let pkgs = import nixpkgs { inherit system; config.allowUnfree = true; };
          in rec {
            # Extra packages available.
            bwrap-seccomp = pkgs.stdenv.mkDerivation {
              pname = "bwrap-seccomp";
              version = "0";
              src = ./bwrap-seccomp.c;
              dontUnpack = true;
              nativeBuildInputs = [ pkgs.pkg-config ];
              buildInputs = [ pkgs.libseccomp ];
              buildPhase = ''$CC $src -o bwrap-seccomp $(pkg-config --cflags --libs libseccomp)'';
              installPhase = ''install -Dm755 bwrap-seccomp $out/bin/bwrap-seccomp'';
            };
            opencode-dev = opencode.packages.${system}.opencode.overrideAttrs (old: {
              preBuild = (old.preBuild or "") + ''
                substituteInPlace packages/opencode/src/cli/cmd/generate.ts \
                  --replace-fail 'const prettier = await import("prettier")' 'const prettier: any = { format: async (s: string) => s }' \
                  --replace-fail 'const babel = await import("prettier/plugins/babel")' 'const babel = {}' \
                  --replace-fail 'const estree = await import("prettier/plugins/estree")' 'const estree = {}'
              '';
            });
          });
      nixosConfigurations.agenthouse = nixosWithOverlay "x86_64-linux" [ ./configuration.nix ];
    };
}
