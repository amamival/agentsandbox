{
  inputs = {
    impermanence.url = "github:nix-community/impermanence";
    impermanence.inputs.nixpkgs.follows = ""; # Only used in tests.
    impermanence.inputs.home-manager.follows = "";
  };

  outputs = { self, impermanence, ... }: {
    nixosModules.default = { lib, pkgs, ... }: {
      imports = [ impermanence.nixosModules.impermanence ];

      options.agentsandbox = {
        mutableSandboxConfig = lib.mkOption { type = lib.types.bool; default = false; };
        memoryMiB = lib.mkOption { type = lib.types.int; default = 8192; };
        vcpus = lib.mkOption { type = lib.types.int; default = 4; };
        portForwards = lib.mkOption {
          type = lib.types.attrsOf (lib.types.submodule {
            options = {
              proto = lib.mkOption { type = lib.types.str; };
              host = lib.mkOption { type = lib.types.int; };
              guest = lib.mkOption { type = lib.types.int; };
            };
          });
          default = { ssh = { proto = "tcp"; host = 2223; guest = 22; }; };
        };
        libvirtXml = lib.mkOption {
          type = lib.types.raw;
          default =
            { pkgs
            , name
            , uuid
            , machineId
            , toplevel
            , kernelParams
            , sysrootNixDir
            , persistentDir
            , runtimeDir
            , memoryMiB
            , vcpus
            , portForwards
            }:
            pkgs.writeText "${name}.xml" ''
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
              '') portForwards}
                    </interface>
                  </devices>
                </domain>
            '';
        };
      };

      config = {
        # Boot.QEMUDirectKernelBoot
        boot.initrd.availableKernelModules = [ "virtio_net" "virtio_pci" "virtio_mmio" "virtio_blk" "virtio_scsi" "9p" "9pnet_virtio" ];
        boot.initrd.kernelModules = [ "virtio_balloon" "virtio_console" "virtio_rng" "virtio_gpu" ];
        boot.loader.external = { enable = true; installHook = "${pkgs.coreutils}/bin/true"; };

        # Boot.AgentSandbox
        fileSystems."/" = { device = "none"; fsType = "tmpfs"; options = [ "mode=755" "nosuid" "nodev" "noexec" ]; };
        fileSystems."/nix" = { device = "nix"; fsType = "virtiofs"; options = [ "nosuid" "nodev" "noexec" ]; };
        boot.kernel.sysctl."vm.overcommit_memory" = lib.mkDefault "1"; # Stability in low memory situations.
        boot.kernelParams = [
          "panic=1" # Since we can't manually respond to a panic, just reboot.
          "boot.panic_on_fail" # Panics on boot failure.
          "systemd.journald.forward_to_console=1" # Show progress while running tests.
        ];

        # Impermanence
        fileSystems."/persistent" = { neededForBoot = true; device = "persistent"; fsType = "virtiofs"; options = [ "nosuid" "nodev" ]; };
        environment.persistence."/persistent" = {
          directories = [ "/var/lib/nixos" ];
          files = [
            # "/etc/machine-id" # kernel command line takes precedence.
            "/etc/ssh/ssh_host_ed25519_key"
          ];
        };
        services.openssh.hostKeys = [{ type = "ed25519"; path = "/etc/ssh/ssh_host_ed25519_key"; }];

        # Networking
        networking.proxy.default = "http://10.0.2.2:3128";
        security.pki.certificates = [
          ''
            -----BEGIN CERTIFICATE-----
            MIIB6zCCAZGgAwIBAgIUaYGGUbLG+HkvXvXu3K82ADTaQKEwCgYIKoZIzj0EAwIw
            HzEdMBsGA1UEAwwUYWdlbnRzYW5kYm94LW1pdG0tY2EwHhcNMjYwNDE2MTIyOTQ4
            WhcNMzYwNDEzMTIyOTQ4WjAfMR0wGwYDVQQDDBRhZ2VudHNhbmRib3gtbWl0bS1j
            YTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABES2mEt0z0slKAmT5c8VjPiIupQ6
            BmDuvsV/2vxcJUW/JAjCoCv5dCdkMqQqx2W2cOqVK13QJlSpNANUlKY8DzKjgaow
            gacwHQYDVR0OBBYEFFxRZJh9Hi9yytkJ+J9m8H9iC3afMB8GA1UdIwQYMBaAFFxR
            ZJh9Hi9yytkJ+J9m8H9iC3afMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQD
            AgGGMBMGA1UdJQQMMAoGCCsGAQUFBwMBMC8GA1UdEQQoMCaCDmF1dGguZG9ja2Vy
            LmlvghRyZWdpc3RyeS0xLmRvY2tlci5pbzAKBggqhkjOPQQDAgNIADBFAiEAjxp4
            swt3eD19q8U5jb7en8Mqdk1i4V+uWRzo9lPafIwCIBHvmTTbmRRSZLtqzHzltolS
            hHRWTSDkZBTwOJ0bWEl7
            -----END CERTIFICATE-----
          ''
        ];

        # Optional firewall
        services.opensnitch.settings = {
          DefaultAction = "deny";
          InterceptUnknown = true;
          Server.Address = "10.0.2.2:50052";
        };

        # Minimize closure size i.e. ${nixpkgs}/nixos/modules/profiles/minimal.nix
        documentation.enable = lib.mkDefault false;
        documentation.doc.enable = lib.mkDefault false;
        documentation.info.enable = lib.mkDefault false;
        documentation.man.enable = lib.mkDefault false;
        documentation.nixos.enable = lib.mkDefault false;
        environment.defaultPackages = lib.mkDefault [ ];
        environment.stub-ld.enable = lib.mkDefault false;
        programs.command-not-found.enable = lib.mkDefault false;
        programs.fish.generateCompletions = lib.mkDefault false;
        services.logrotate.enable = lib.mkDefault false;
        services.udisks2.enable = lib.mkDefault false;
        xdg.autostart.enable = lib.mkDefault false;
        xdg.icons.enable = lib.mkDefault false;
        xdg.mime.enable = lib.mkDefault false;
        xdg.sounds.enable = lib.mkDefault false;
      };
    };
  };
}
