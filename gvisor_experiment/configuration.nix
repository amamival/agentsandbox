{ config, lib, pkgs, pkgs-unstable, modulePath, sessionVm ? false, hostUid ? 1000, hostGid ? 100, ... }:
let
  HostConf = {
    hostName = if sessionVm then "agenthouse-virt" else "agenthouse";
    wheelUser = "vscode";
    primaryGroup = "users";
    uid = hostUid;
    gid = hostGid;
  };
  Boot.Mode = if sessionVm then {
    # boot.isContainer = true;
    # boot.postBootCommands = ''
    #   # `nixos-rebuild` also requires a "system" profile.
    #   ${config.nix.package.out}/bin/nix-env -p /nix/var/nix/profiles/system --set /run/current-system
    # '';
    boot.isContainer = false;
    boot.loader.external.enable = true;
    boot.loader.external.installHook = pkgs.writeShellScript "virt-install-bootloader" ''
      exit 0
    '';
    boot.initrd.systemd.enable = true;
    boot.initrd.availableKernelModules = [ "virtio_pci" "virtiofs" "virtio_net" ];
    boot.kernelParams = [
      "console=ttyS0"
      "systemd.journald.forward_to_console=1"
      "systemd.unit=multi-user.target"
    ];
    fileSystems."/" = {
      device = "none";
      fsType = "tmpfs";
      options = [ "mode=755" ];
    };
    fileSystems."/nix" = {
      device = "nix";
      fsType = "virtiofs";
      neededForBoot = true;
      options = [ "ro" ];
    };
    fileSystems."/persistent" = {
      device = "persistent";
      fsType = "virtiofs";
      neededForBoot = true;
    };
    fileSystems."/workspace" = {
      device = "workspace";
      fsType = "virtiofs";
    };
  } else {
    boot.isContainer = true;
    boot.postBootCommands = ''
      # `nixos-rebuild` also requires a "system" profile.
      ${config.nix.package.out}/bin/nix-env -p /nix/var/nix/profiles/system --set /run/current-system
    '';
    # Install new init script.
    # system.activationScripts.installInitScript = "ln -fs $systemConfig/init /init";
  };
  Boot.Minimal = {
    documentation = {
      enable = false;
      doc.enable = false;
      info.enable = false;
      man.enable = false;
      nixos.enable = false;
    };
    environment = {
      defaultPackages = [ ];
      stub-ld.enable = false;
    };
    programs = {
      command-not-found.enable = false;
      fish.generateCompletions = false;
    };
    services = {
      logrotate.enable = false;
      udisks2.enable = false;
    };
    xdg = {
      autostart.enable = false;
      icons.enable = false;
      mime.enable = false;
      sounds.enable = false;
    };
  };
  Boot.Impermanence = {
    environment.persistence."/persistent" = {
      hideMounts = true;
      allowTrash = true;
      directories = [
        "/etc/nixos"
        "/var/lib/nixos"
        "/var/log"
        { directory = "/workspace"; user = HostConf.wheelUser; group = HostConf.primaryGroup; mode = "u=rwx,g=rx,o="; }
      ] ++ lib.optionals sessionVm [
        "/var/lib/libvirt"
      ];
      files = [
        "/etc/machine-id"
        "/etc/ssh/ssh_host_ed25519_key"
        # { file = ""; parentDirectory = { mode = "u=rwx,g=,o="; ... }; }
      ];
    };
  };
  BootConfigs = builtins.attrValues Boot;
  
  Networking = {
    networking.hostName = HostConf.hostName;
  } // lib.optionalAttrs (!sessionVm) {
    # networking.resolvconf.enable = false; # Provided by host.
    networking.resolvconf.enable = false;
  } // lib.optionalAttrs sessionVm {
    networking.useDHCP = lib.mkDefault true;
  };

  Shell = {
    environment.systemPackages = with pkgs; [
      btop
      dig
      htop
      file
      git
      gh
      iproute2
      jq
      lsof
      nixpkgs-fmt
      nodejs_24 # For Remote-SSH: "vscode@localhost -p 2222".
      perf
      ripgrep
      shellcheck
      sqlite
      strace
      sysstat
      taplo
      tmux
      util-linux
      yq
      wget
    ] ++ lib.optionals sessionVm [
      libvirt
      qemu_kvm
    ];
    environment.shellAliases = {
      ll = "ls -l";
      ga = "git add";
      gb = "git branch";
      gc = "git commit";
      gck = "git checkout";
      gd = "git diff";
      gl = "git log";
      gr = "git restore";
      gs = "git status";
      gsw = "git switch";
      gpl = "git pull";
      gps = "git push";
    };
    environment.variables.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = "1";
  };

  DesktopEnvironment = { };

  Service.OpenSSH = {
    security.pam.services.sshd.allowNullPassword = true;
    services.openssh = {
      enable = true;
      settings = {
        PermitEmptyPasswords = true;
        PermitRootLogin = "yes";
      };
      hostKeys = [{ type = "ed25519"; path = "/etc/ssh/ssh_host_ed25519_key"; }];
    };
  };
  Service.QemuGuest = lib.mkIf sessionVm {
    services.qemuGuest.enable = true;
  };
  Service.Libvirt = lib.mkIf sessionVm {
    virtualisation.libvirtd.enable = true;
  };
  Services = builtins.attrValues Service;

  Users = {
    users.mutableUsers = false;
    users.allowNoPasswordLogin = true;
    users.users.${HostConf.wheelUser} = {
      isNormalUser = true;
      uid = HostConf.uid;
      group = HostConf.primaryGroup;
      initialPassword = "";
      extraGroups = [ "wheel" "systemd-journal" ]
        ++ lib.optionals sessionVm [ "libvirtd" "kvm" ];
    };
    security.sudo.wheelNeedsPassword = false;

    home-manager.users.${HostConf.wheelUser} = { pkgs, ... }: {
      programs.bash.enable = true;
      programs.bash.profileExtra = ''
        export NPM_CONFIG_PREFIX=~/.npm-global PATH=$PATH:~/.npm-global/bin
      '';
      programs.direnv = {
        enable = true;
        nix-direnv.enable = true;
        config = {
          global.warn_timeout = "30m"; # `nix develop` may take a long time.
          whitelist.prefix = [ "/" ]; # Since we are running in container / VM.
        };
      };
      systemd.user.services.ai-cli-update = {
        Unit.Description = "Install or update codex and claude on login";
        Service = {
          Type = "oneshot";
          ExecStart = toString (pkgs.writeShellScript "ai-cli-update" ''
            NPM_CONFIG_PREFIX=~/.npm-global ${pkgs.nodejs}/bin/npm install -g \
            @openai/codex@latest @anthropic-ai/claude-code@latest
          '');
        };
        Install.WantedBy = [ "default.target" ];
      };

      home.persistence."/persistent" = {
        directories = [
          ".cache/pip"
          ".cache/pre-commit"
          ".cache/claude-cli-nodejs"
          ".cache/go-build"
          ".cache/go-mod"
          ".cache/mise"
          ".cargo"
          ".claude"
          ".codex"
          ".config/Claude"
          ".config/github-copilot"
          ".config/mise"
          ".config/nvim"
          ".config/ohmyposh"
          ".config/opencode"
          ".config/tmux/plugins"
          ".local/bin"
          ".local/share/claude"
          ".local/share/fish"
          ".local/share/mise"
          ".local/state/mise"
          ".npm-global"
          ".pnpm-store"
          ".oh-my-zsh"
          ".poshthemes"
          ".rustup"
          ".tmux/plugins"
          "go"
          { directory = ".gnupg"; mode = "0700"; }
          { directory = ".ssh"; mode = "0700"; }
          { directory = ".local/share/keyrings"; mode = "0700"; }
        ];
        files = [
          ".bash_history"
          ".claude.json"
          ".claude.json.backup"
          ".python_history"
          ".zsh_history"
        ];
      };
      home.stateVersion = config.system.stateVersion;
    };
  };

  System = {
    time.timeZone = "Asia/Tokyo";
    i18n.supportedLocales = [ "ja_JP.UTF-8/UTF-8" "en_US.UTF-8/UTF-8" ];
    i18n.defaultLocale = "ja_JP.UTF-8";
  };

  NixOS = {
    nix.gc.automatic = true;
    nix.gc.dates = "Monday 04:00";
    nix.settings.auto-optimise-store = true;
    nix.settings.experimental-features = "flakes nix-command";
    nix.settings.max-jobs = "auto";
    system.stateVersion = "25.11";
  };
in
{
  imports = BootConfigs ++ Services ++
    [ Networking Shell DesktopEnvironment Users System NixOS ];
}
