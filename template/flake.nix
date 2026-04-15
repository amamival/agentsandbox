{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager/release-25.11";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
    impermanence.url = "github:nix-community/impermanence";
    impermanence.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, nixpkgs-unstable, home-manager, impermanence, ... }:
    let
      lib = nixpkgs.lib;
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfree = true;
      };
      guestSystem = nixpkgs.lib.nixosSystem {
        inherit system;
        specialArgs = {
          pkgs-unstable = import nixpkgs-unstable { inherit system; config.allowUnfree = true; };
          inherit nixpkgs;
        };
        modules = [
          home-manager.nixosModules.home-manager
          impermanence.nixosModules.impermanence
          ./configuration.nix
        ];
      };
    in
    {
      nixosConfigurations.default = guestSystem;

      sandboxConfigurations.default = {
        nixosConfiguration = "default";
        mutableSandboxConfig = false;
        memoryMiB = 8192;
        vcpus = 4;
        portForwards = [
          {
            proto = "tcp";
            host = 2223;
            guest = 22;
          }
        ];
        libvirtXml = {
          pkgs,
          name,
          uuid,
          machineId,
          toplevel,
          kernelParams,
          sysrootNixDir,
          persistentDir,
          runtimeDir,
          memoryMiB,
          vcpus,
          portForwards,
        }: pkgs.writeText "agentsandbox-${name}.xml" ''
          <domain type='kvm'>
            <name>${name}</name>
            <uuid>${uuid}</uuid>
            <memory unit='MiB'>${toString memoryMiB}</memory>
            <currentMemory unit='MiB'>${toString memoryMiB}</currentMemory>
            <vcpu placement='static'>${toString vcpus}</vcpu>
            <os>
              <type arch='x86_64' machine='q35'>hvm</type>
              <kernel>${toplevel}/kernel</kernel>
              <initrd>${toplevel}/initrd</initrd>
              <cmdline>${lib.escapeXML (lib.removeSuffix "\n" kernelParams)} systemd.machine_id=${machineId}</cmdline>
            </os>
            <cpu mode='host-passthrough' migratable='off'/>
            <memoryBacking>
              <source type='memfd'/>
              <access mode='shared'/>
            </memoryBacking>
            <features>
              <acpi/>
              <apic/>
            </features>
            <devices>
              <filesystem type='mount' accessmode='passthrough'>
                <driver type='virtiofs' queue='1024'/>
                <source socket='${runtimeDir}/virtiofs/nix.sock'/>
                <target dir='nix'/>
              </filesystem>
              <filesystem type='mount' accessmode='passthrough'>
                <driver type='virtiofs' queue='1024'/>
                <source socket='${runtimeDir}/virtiofs/persistent.sock'/>
                <target dir='persistent'/>
              </filesystem>
              <serial type='pty'>
                <target port='0'/>
              </serial>
              <console type='pty'>
                <target type='serial' port='0'/>
              </console>
              <rng model='virtio'>
                <backend model='random'>/dev/urandom</backend>
              </rng>
              <interface type='user'>
                <backend type='passt'/>
                <model type='virtio'/>
          ${lib.concatMapStrings (forward: ''
                  <portForward proto='${forward.proto}'>
                    <range start='${toString forward.host}' to='${toString forward.guest}'/>
                  </portForward>
          '') portForwards}      </interface>
            </devices>
          </domain>
        '';
      };
    };
}
