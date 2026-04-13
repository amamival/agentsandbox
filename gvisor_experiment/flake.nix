{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager/release-25.11";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
    impermanence.url = "github:nix-community/impermanence";
    impermanence.inputs.nixpkgs.follows = "";
    impermanence.inputs.home-manager.follows = "";
  };
  outputs = { self, nixpkgs, nixpkgs-unstable, home-manager, impermanence }:
    let
      extraPackagesFor = system:
        let pkgs = import nixpkgs { inherit system; config.allowUnfree = true; };
        in {
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
        };
      nixosWithOverlay = { system, modules, specialArgs ? { } }:
        nixpkgs.lib.nixosSystem {
          inherit system;
          modules = modules ++ [
            home-manager.nixosModules.home-manager
            impermanence.nixosModules.impermanence
            (_: { nixpkgs.overlays = [ (_: _: extraPackagesFor system) ]; })
          ];
          specialArgs = specialArgs // {
            pkgs-unstable = import nixpkgs-unstable { inherit system; config.allowUnfree = true; };
          };
        };
      eachSystem = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
    in
    {
      packages = eachSystem (system:
        let pkgs = import nixpkgs { inherit system; config.allowUnfree = true; };
            extraPackages = extraPackagesFor system;
            virtConfig = nixosWithOverlay {
              inherit system;
              modules = [ ./configuration.nix ];
              specialArgs = {
                sessionVm = true;
                hostUid = 1000;
                hostGid = 100;
              };
            };
        in extraPackages // {
        virt-experiment-artifacts = pkgs.runCommand "virt-experiment-artifacts" { } ''
          mkdir -p "$out"
          ln -s ${virtConfig.config.boot.kernelPackages.kernel}/${virtConfig.config.system.boot.loader.kernelFile} "$out/kernel"
          ln -s ${virtConfig.config.system.build.initialRamdisk}/${virtConfig.config.system.boot.loader.initrdFile} "$out/initrd"
          ln -s ${virtConfig.config.system.build.toplevel}/kernel-params "$out/kernel-params"
          ln -s ${virtConfig.config.system.build.toplevel} "$out/system"
        '';
      });
      devShells = eachSystem (system:
        let
          pkgs = import nixpkgs { inherit system; config.allowUnfree = true; };
          expPackages = with pkgs; [
            bash
            coreutils
            curl
            gawk
            gnugrep
            gnutar
            gvisor
            jq
            shellcheck
            sudo
            util-linux
          ];
        in {
          default = pkgs.mkShell {
            packages = expPackages;
            shellHook = ''
              echo "gVisor experiment shell ready"
              echo "run: ./experiment.sh preflight"
            '';
          };
          gvisor-host = pkgs.mkShell {
            packages = expPackages;
          };
          virt-host = pkgs.mkShell {
            packages = with pkgs; [ libvirt qemu_kvm virtiofsd passt openssh iproute2 ];
            LIBVIRT_DEFAULT_URI = "qemu:///session";
          };
        });
      nixosConfigurations.agenthouse = nixosWithOverlay {
        system = "x86_64-linux";
        modules = [ ./configuration.nix ];
        specialArgs = {
          sessionVm = false;
          hostUid = 1000;
          hostGid = 100;
        };
      };
      nixosConfigurations.agenthouse-virt-session = nixosWithOverlay {
        system = "x86_64-linux";
        modules = [ ./configuration.nix ];
        specialArgs = {
          sessionVm = true;
          hostUid = 1000;
          hostGid = 100;
        };
      };
    };
}
