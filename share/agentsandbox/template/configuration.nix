{ config, lib, pkgs, pkgs-unstable, nixpkgs, ... }:
let
  HostConf = {
    hostName = "agentsandbox";
    wheelUser = "vscode";
  };

  Boot.QEMU = {
    virtualisation.cores = lib.mkDefault 4;
    virtualisation.memorySize = lib.mkDefault 8192;
  };
  Boot.Impermanence = {
    environment.persistence."/persistent" = {
      hideMounts = true;
      allowTrash = true;
      directories = [
        "/etc/nixos"
        "/var/log"
        # { directory = "/workspace"; user = HostConf.wheelUser; group = "users"; mode = "u=rwx,g=rx,o="; }
      ];
      files = [
        # { file = ""; parentDirectory = { mode = "u=rwx,g=,o="; ... }; }
      ];
    };
  };
  BootConfigs = builtins.attrValues Boot;

  Networking = {
    networking.hostName = HostConf.hostName;
    networking.firewall.enable = false;
    services.opensnitch.enable = false;
  };

  Shell = {
    environment.systemPackages = with pkgs; [
      btop
      dig
      file
      git
      gh
      htop
      iproute2
      jq
      lsof
      nixpkgs-fmt
      nodejs_24
      ripgrep
      shellcheck
      sqlite
      strace
      sysstat
      taplo
      tmux
      util-linux
      wget
      yq
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
      settings.PermitEmptyPasswords = true;
      settings.PermitRootLogin = "yes";
    };
  };
  Services = builtins.attrValues Service;

  Users = {
    users.mutableUsers = false;
    users.users.${HostConf.wheelUser} = {
      isNormalUser = true;
      uid = 1000;
      initialPassword = "";
      extraGroups = [ "wheel" "systemd-journal" ];
      subUidRanges = [{ startUid = 32768; count = 32768; }];
      subGidRanges = [{ startGid = 32768; count = 32768; }];
    };
    security.sudo.wheelNeedsPassword = false;

    home-manager.users.${HostConf.wheelUser} = { pkgs, ... }: {
      programs.bash.enable = true;
      programs.bash.profileExtra = "export NPM_CONFIG_PREFIX=~/.npm-global PATH=$PATH:~/.npm-global/bin";
      programs.direnv = {
        enable = true;
        nix-direnv.enable = true;
        config.global.warn_timeout = "30m"; # `nix develop` may take a long time.
        config.whitelist.prefix = [ "/" ]; # Since we are running in a VM.
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
    # Adjust to your environment.
    # i18n.defaultLocale = "ja_JP.UTF-8";
    # i18n.supportedLocales = [ "ja_JP.UTF-8/UTF-8" "en_US.UTF-8/UTF-8" ];
    # time.timeZone = "Asia/Tokyo";
  };

  NixOS = {
    nix.channel.enable = false; # In favor of Nix Flakes.
    nix.gc.automatic = true;
    nix.gc.dates = "Monday 04:00";
    nix.settings.auto-optimise-store = true;
    nix.settings.experimental-features = [ "nix-command" "flakes" ];
    nix.settings.max-jobs = "auto";
    nix.settings.system-features = [ "nixos-test" ];
    nix.settings.trusted-users = [ HostConf.wheelUser ];
    system.stateVersion = "25.11";
  };
in
{
  imports = BootConfigs ++ Services ++
    [ Networking Shell DesktopEnvironment Users System NixOS ];
}
