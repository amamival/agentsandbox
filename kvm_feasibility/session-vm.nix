{ lib, sessionVm ? false, ... }:
lib.mkIf sessionVm {
  systemd.services."home-manager-vscode".enable = lib.mkForce false;
}
