{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    impermanence.url = "github:nix-community/impermanence";
    impermanence.inputs.nixpkgs.follows = ""; # Only used in tests.
    impermanence.inputs.home-manager.follows = "";
  };

  outputs = { self, nixpkgs, impermanence, ... }: {
    nixosModules.default = { config, lib, pkgs, ... }: {
      imports = [
        "${nixpkgs.outPath}/nixos/modules/virtualisation/qemu-vm.nix"
        impermanence.nixosModules.impermanence
      ];

      options.agentsandbox = {
        mutableSandboxConfig = lib.mkOption { type = lib.types.bool; default = false; };
        portForwards = lib.mkOption {
          type = lib.types.attrsOf (lib.types.submodule {
            options = {
              proto = lib.mkOption { type = lib.types.enum [ "tcp" "udp" ]; };
              host = lib.mkOption {
                type = lib.types.addCheck
                  (lib.types.coercedTo lib.types.int
                    (n: { start = n; end = n; })
                    (lib.types.submodule ({ config, ... }: {
                      options = {
                        start = lib.mkOption { type = lib.types.int; description = "Host-side port or range start."; };
                        end = lib.mkOption { type = lib.types.int; description = "Host-side range end (inclusive)."; };
                      };
                      config.end = lib.mkDefault config.start;
                    })))
                  (h: h.start <= h.end);
                description = "Published host port(s). Int or { start, end } inclusive range.";
              };
              guest = lib.mkOption {
                type = lib.types.int;
                description = "Guest port matching host start (libvirt range to; parallel offset for ranges).";
              };
            };
          });
          default = { ssh = { proto = "tcp"; host = 2223; guest = 22; }; };
        };
      };

      config = {
        # Boot.QEMUDirectKernelBoot
        virtualisation.directBoot.enable = true;
        virtualisation.diskImage = null;
        #virtualisation.fileSystems = lib.mkForce { };
        #virtualisation.fileSystems = lib.mkForce config.fileSystems;
        virtualisation.sharedDirectories = lib.mkForce { };
        virtualisation.mountHostNixStore = false;
        virtualisation.useDefaultFilesystems = false;
        virtualisation.useNixStoreImage = false;
        virtualisation.useBootLoader = false;
        system.systemBuilderCommands =
          let
            kernelParams = lib.escapeXML (lib.concatStringsSep " " config.boot.kernelParams);
            portForwards = lib.mapAttrsToList (name: forward: "${name}\t${forward.proto}\t${toString forward.host.start}\t${toString forward.host.end}\t${toString forward.guest}") config.agentsandbox.portForwards;
            portForwardsFile = pkgs.writeText "port-forwards" (lib.concatStringsSep "\n" portForwards + "\n");
            portForwardsXml = lib.concatMapStrings
              (f: ''
                <portForward proto='${lib.escapeXML forward.proto}'>${
                if f.host.start == f.host.end then
                  "<range start='${toString f.host.start}' to='${toString g}'/>"
                else
                  "<range start='${toString f.host.start}' end='${toString f.host.end}' to='${toString f.guest}'/>"
                }</portForward>
              '')
              (lib.attrValues config.agentsandbox.portForwards);
            libvirtDomainXmlGen = pkgs.writeShellScript "domain.xml.sh" ''
              TOPLEVEL="$(cd -- "$(dirname -- "$0")" && pwd -P)"
              SYSROOT="''${NIX_DIR%/nix}"
              KERNEL="$(readlink "$TOPLEVEL/kernel")"
              INITRD="$(readlink "$TOPLEVEL/initrd")"
              UID_IDMAP_XML="$(
                  while read -r START TARGET COUNT; do
                    echo "                    <uid start='$START' target='$TARGET' count='$COUNT'/>"
                  done <<<"$UID_MAP"
                )"
              GID_IDMAP_XML="$(
                  while read -r START TARGET COUNT; do
                    echo "                    <gid start='$START' target='$TARGET' count='$COUNT'/>"
                  done <<<"$GID_MAP"
                )"
              [[ "$KERNEL" == /* ]] && KERNEL="$SYSROOT$KERNEL" || KERNEL="$TOPLEVEL/$KERNEL"
              [[ "$INITRD" == /* ]] && INITRD="$SYSROOT$INITRD" || INITRD="$TOPLEVEL/$INITRD"
              cat <<EOF
              <domain type='kvm'>
                <name>$INSTANCE_ID</name>
                <uuid>$DOMAIN_UUID</uuid>
                <memory unit='MiB'>${toString config.virtualisation.memorySize}</memory>
                <currentMemory unit='MiB'>${toString config.virtualisation.memorySize}</currentMemory>
                <vcpu placement='static'>${toString config.virtualisation.cores}</vcpu>
                <os>
                  <type arch='x86_64' machine='q35'>hvm</type>
                  <kernel>$KERNEL</kernel>
                  <initrd>$INITRD</initrd>
                  <cmdline>${kernelParams} init=/nix/var/nix/profiles/system/init systemd.machine_id=$MACHINE_ID</cmdline>
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
                  <filesystem type='mount'>
                    <driver type='virtiofs' queue='1024'/>
                    <binary path='${pkgs.virtiofsd}/bin/virtiofsd' xattr='on'>
                      <cache mode='always'/>
                      <sandbox mode='namespace'/>
                      <!-- Rust virtiofsd 1.13.x does not advertise lock support to libvirt:
                            https://virtio-fs.gitlab.io/virtiofsd/doc/virtiofsd/fuse/struct.FsOptions.html -->
                      <thread_pool size='0'/>
                    </binary>
                    <source dir='$NIX_DIR'/>
                    <target dir='nix'/>
                    <idmap>
              $UID_IDMAP_XML
              $GID_IDMAP_XML
                    </idmap>
                  </filesystem>
                  <filesystem type='mount'>
                    <driver type='virtiofs' queue='1024'/>
                    <source socket='$PERSISTENT_SOCKET_XML'/>
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
              ${portForwardsXml}
                  </interface>
                </devices>
              </domain>
              EOF
            '';
          in
          lib.mkAfter ''
            cp ${libvirtDomainXmlGen} "$out/domain.xml.sh"
            cp ${portForwardsFile} "$out/port-forwards"
            ${lib.optionalString config.agentsandbox.mutableSandboxConfig ''
              touch "$out/mutable-sandbox-config"
            ''}
          '';
        # Keep initrd module set minimal; root and early mounts only need virtiofs here.
        boot.initrd.kernelModules = [ "virtiofs" ];
        #boot.loader.external = { enable = true; installHook = "${pkgs.coreutils}/bin/true"; };

        # Boot.AgentSandbox
        virtualisation.fileSystems."/" = { device = "none"; fsType = "tmpfs"; options = [ "mode=755" "nosuid" "nodev" "noexec" ]; };
        virtualisation.fileSystems."/nix" = { device = "nix"; fsType = "virtiofs"; options = [ "nosuid" "nodev" ]; };
        boot.kernel.sysctl."vm.overcommit_memory" = lib.mkDefault "1"; # Stability in low memory situations.
        boot.kernelParams = [
          "panic=1" # Since we can't manually respond to a panic, just reboot.
          "boot.panic_on_fail" # Panics on boot failure.
          "systemd.journald.forward_to_console=1" # Show progress while running tests.
          "console=ttyS0,115200n8" # Used by console subcommand.
        ];
        services.getty.autologinUser = "root";

        # Service.OpenSSH, used by exec/ssh subcommand.
        security.pam.services.sshd.allowNullPassword = true;
        services.openssh = {
          enable = true;
          settings.PermitEmptyPasswords = true;
          settings.PermitRootLogin = "yes";
        };
        users.allowNoPasswordLogin = true;


        # Impermanence
        virtualisation.fileSystems."/persistent" = { neededForBoot = true; device = "persistent"; fsType = "virtiofs"; options = [ "nosuid" "nodev" ]; };
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

