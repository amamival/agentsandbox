{ config, lib, pkgs, pkgs-unstable, modulePath, ... }:
let
  HostConf = {
    hostName = "agenthouse";
    wheelUser = "vscode";
  };
  inherit (lib) mkDefault;
  Boot.Minimal = {
    documentation = {
      enable = mkDefault false;
      doc.enable = mkDefault false;
      info.enable = mkDefault false;
      man.enable = mkDefault false;
      nixos.enable = mkDefault false;
    };

    environment = {
      # Perl is a default package.
      defaultPackages = mkDefault [ ];
      stub-ld.enable = mkDefault false;
    };

    programs = {
      command-not-found.enable = mkDefault false;
      fish.generateCompletions = mkDefault false;
    };

    services = {
      logrotate.enable = mkDefault false;
      udisks2.enable = mkDefault false;
    };

    xdg = {
      autostart.enable = mkDefault false;
      icons.enable = mkDefault false;
      mime.enable = mkDefault false;
      sounds.enable = mkDefault false;
    };
  };
  Boot.Container = {
    boot.isContainer = true;
    boot.postBootCommands = ''
      # `nixos-rebuild` also requires a "system" profile.
      ${config.nix.package.out}/bin/nix-env -p /nix/var/nix/profiles/system --set /run/current-system
    '';
    # Install new init script.
    system.activationScripts.installInitScript = "ln -fs $systemConfig/init /init";
  };
  BootConfigs = builtins.attrValues Boot;
  Networking = {
    networking.hostName = HostConf.hostName;
    systemd.resolveconf.enable = false; # Provided by wrapper.
  };

  Users = {
    users.mutableUsers = false;
    users.allowNoPasswordLogin = true;
    users.users.${HostConf.wheelUser} = {
      isNormalUser = true;
      uid = 1000;
      extraGroups = [ "wheel" ];
    };
  };

  Shell = {
    environment.systemPackages = with pkgs; [
      #btop
      #htop
      git
      iproute2
      #lsof
      jq
      util-linux
      #shellcheck
      #wget
    ];
  };
  DesktopEnvironment = { };
  Service.None = { };
  Services = builtins.attrValues Service;
  System = { };
  NixOS = {
    system.stateVersion = "25.11";
  };
in
{
  imports = BootConfigs ++ Services ++
    [ Networking Users Shell DesktopEnvironment System NixOS ];
}
