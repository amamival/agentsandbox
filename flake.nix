{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager/release-25.11";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
    impermanence.url = "github:nix-community/impermanence";
    impermanence.inputs.nixpkgs.follows = "";
  };
  outputs = { self, nixpkgs, nixpkgs-unstable, home-manager, impermanence, ... }:
    let
      eachSystem = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
      pkgsFor = system: import nixpkgs { inherit system; config.allowUnfree = true; };
    in
    {
      packages = eachSystem (system:
        let pkgs = pkgsFor system; in {
          default = pkgs.stdenvNoCC.mkDerivation {
            pname = "agentsandbox";
            version = "0.0.0-dev";
            src = self;
            dontUnpack = true;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            installPhase = ''
              install -Dm755 ${./agentsandbox} "$out/bin/agentsandbox"
              wrapProgram "$out/bin/agentsandbox" \
                --prefix PATH : ${pkgs.lib.makeBinPath (with pkgs; [
                  bubblewrap curl jq libvirt mitmproxy openssh socat util-linux virtiofsd zstd
                ])}
              install -d "$out/share/agentsandbox"
              cp -a ${./template} ${./mitm-proxy-ca.pem} ${./mitm-proxy-ca.key} "$out/share/agentsandbox/"
            '';
          };
        });

      apps = eachSystem (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/agentsandbox";
          meta.description = "Manage isolated NixOS VM sandboxes for agentic workflows";
        };
      });

      devShells = eachSystem (system:
        let pkgs = pkgsFor system; in {
          default = pkgs.mkShell {
            packages = with pkgs; [ bubblewrap curl jq libvirt mitmproxy openssh passt qemu_kvm socat util-linux virtiofsd zstd ];
            LIBVIRT_DEFAULT_URI = "qemu:///session";
          };
        });

      checks = eachSystem (system:
        let pkgs = pkgsFor system; in {
          lint = pkgs.runCommand "lint" { } ''${pkgs.shellcheck}/bin/shellcheck ${./agentsandbox} > "$out"'';

          nixos-userns-smoke = pkgs.testers.runNixOSTest {
            name = "agentsandbox-nixos-userns-smoke";
            nodes.machine = { pkgs, ... }: {
              security.unprivilegedUsernsClone = true;
              security.wrappers.newuidmap.source = "${pkgs.shadow}/bin/newuidmap";
              security.wrappers.newuidmap.setuid = true;
              security.wrappers.newgidmap.source = "${pkgs.shadow}/bin/newgidmap";
              security.wrappers.newgidmap.setuid = true;
              users.users.vscode = {
                isNormalUser = true;
                subUidRanges = [{ startUid = 165536; count = 65536; }];
                subGidRanges = [{ startGid = 165536; count = 65536; }];
              };
              environment.systemPackages = with pkgs; [ util-linux shadow ];
            };
            testScript = ''
              start_all()
              machine.wait_for_unit("multi-user.target")
              machine.succeed("su -s /bin/sh -c 'unshare --map-auto --setuid=0 --setgid=0 true' vscode")
            '';
          };

          nixos-e2e = pkgs.testers.runNixOSTest {
            name = "agentsandbox-nixos-e2e";
            nodes.machine = { pkgs, ... }: {
              security.unprivilegedUsernsClone = true;
              security.wrappers.newuidmap = {
                source = "${pkgs.shadow}/bin/newuidmap";
                setuid = true;
                owner = "root";
                group = "root";
              };
              security.wrappers.newgidmap = {
                source = "${pkgs.shadow}/bin/newgidmap";
                setuid = true;
                owner = "root";
                group = "root";
              };
              users.users.root = {
                subUidRanges = [{ startUid = 100000; count = 65536; }];
                subGidRanges = [{ startGid = 100000; count = 65536; }];
              };
              security.pki.certificateFiles = [ ./mitm-proxy-ca.pem ];
              virtualisation.additionalPaths = [
                (toString nixpkgs)
                (toString nixpkgs-unstable)
                (toString home-manager)
                (toString impermanence)
              ];
              virtualisation.diskSize = 65536;
              virtualisation.memorySize = 12288;
              virtualisation.cores = 6;
              networking.interfaces.eth1 = {
                useDHCP = false;
                ipv4.addresses = [{ address = "192.168.1.1"; prefixLength = 24; }];
              };
              networking.hosts."192.168.1.2" = [ "auth.docker.io" "registry-1.docker.io" ];
              nix.settings.experimental-features = [ "nix-command" "flakes" ];
              environment.variables.XDG_RUNTIME_DIR = "/run/user/0";
              environment.systemPackages = with pkgs; [
                self.packages.${system}.default
                passt
                qemu_kvm
                zstd
                nix
                git
                coreutils
                gawk
                gnugrep
                gnused
                findutils
              ];
              virtualisation.libvirtd.enable = true;
            };
            nodes.registry = { pkgs, ... }: {
              networking.firewall.enable = false;
              networking.interfaces.eth1 = {
                useDHCP = false;
                ipv4.addresses = [{ address = "192.168.1.2"; prefixLength = 24; }];
              };
              services.nginx.enable = true;
              services.nginx.virtualHosts."auth.docker.io" = {
                onlySSL = true;
                sslCertificate = ./mitm-proxy-ca.pem;
                sslCertificateKey = ./mitm-proxy-ca.key;
                locations."= /token".return = ''200 '{"token":"test"}' '';
              };
              services.nginx.virtualHosts."registry-1.docker.io" = {
                onlySSL = true;
                sslCertificate = ./mitm-proxy-ca.pem;
                sslCertificateKey = ./mitm-proxy-ca.key;
                locations."= /v2/nixos/nix/blobs/0".alias = pkgs.runCommand "docker-image-nix-layer" { } ''
                  tar xf ${pkgs.dockerTools.examples.nix} --wildcards '*/layer.tar' -O >layer.tar
                  mkdir -p work/nix/var/nix/profiles
                  ln -sfn / work/nix/var/nix/profiles/default
                  (cd work && ${pkgs.gnutar}/bin/tar -rf ../layer.tar .)
                  ${pkgs.gzip}/bin/gzip -9n <layer.tar >"$out"
                '';
                locations."= /v2/nixos/nix/manifests/latest".return = ''200 '{"manifests":[{"digest":"main","platform":{"architecture":"amd64","os":"linux"}}]}' '';
                locations."= /v2/nixos/nix/manifests/main".return = ''200 '{"layers":[{"digest":"0"}]}' '';
              };
            };
            testScript = ''
              start_all()
              registry.wait_for_unit("nginx.service")
              machine.succeed("curl -fsS https://auth.docker.io/token | grep -q test")
              machine.succeed("curl -fsS https://registry-1.docker.io/v2/nixos/nix/manifests/latest | grep -q manifest")
              machine.succeed("curl -fsS https://registry-1.docker.io/v2/nixos/nix/manifests/main | grep -q layer")
              machine.succeed("curl -fsS --head https://registry-1.docker.io/v2/nixos/nix/blobs/0")

              machine.wait_for_unit("libvirtd.service")
              machine.succeed("install -d /run/user/0")
              machine.succeed("agentsandbox init")
              machine.succeed("mkdir -p .agentsandbox/_pin .agentsandbox/agentsandbox/_pin")
              machine.succeed(
                  "cp -a ${toString nixpkgs} .agentsandbox/_pin/nixpkgs && "
                  + "cp -a ${toString nixpkgs-unstable} .agentsandbox/_pin/nixpkgs-unstable && "
                  + "cp -a ${toString home-manager} .agentsandbox/_pin/home-manager && "
                  + "cp -a ${toString impermanence} .agentsandbox/agentsandbox/_pin/impermanence"
              )
              machine.succeed(
                  "sed -i "
                  + "-e 's|github:NixOS/nixpkgs/nixos-25.11|path:./_pin/nixpkgs|' "
                  + "-e 's|github:NixOS/nixpkgs/nixos-unstable|path:./_pin/nixpkgs-unstable|' "
                  + "-e 's|github:nix-community/home-manager/release-25.11|path:./_pin/home-manager|' "
                  + ".agentsandbox/flake.nix"
              )
              machine.succeed(
                  "sed -i "
                  + "-e 's|github:nix-community/impermanence|path:./_pin/impermanence|' "
                  + ".agentsandbox/agentsandbox/flake.nix"
              )
              machine.succeed("nix flake lock .agentsandbox")
              machine.succeed("agentsandbox help | grep -F proxy-logs >/dev/null")
              machine.succeed("agentsandbox version | grep -E '^[^[:space:]]+$' >/dev/null")
              machine.succeed("agentsandbox doctor | grep -F uri: >/dev/null")
              machine.succeed(
                  "eval \"$(agentsandbox __resolve-active-config \"$PWD\")\" && "
                  + "[[ \"$ACTIVE_CONFIG_DIR\" == \"$PWD/.agentsandbox\" ]]"
              )
              machine.succeed(
                  "eval \"$(agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" agenthouse)\" && "
                  + "[[ \"$INSTANCE_ID\" == \"''${INSTANCE_NAME}-''${MACHINE_ID}\" ]]"
              )
              machine.succeed("agentsandbox allow-domain Example.COM")
              machine.succeed("agentsandbox allow-domain https://example.com/path")
              machine.succeed("agentsandbox allow-domain 'https://*.Example.COM.:8443/path'")
              machine.succeed("[[ \"$(grep -Fxc example.com .agentsandbox/allowed_hosts)\" == 1 ]]")
              machine.succeed("grep -Fx '*.example.com' .agentsandbox/allowed_hosts >/dev/null")
              machine.succeed("agentsandbox unallow-domain https://EXAMPLE.com.:443/path")
              machine.succeed("! grep -Fx example.com .agentsandbox/allowed_hosts >/dev/null")
              machine.succeed("mkdir -p alpha beta")
              machine.succeed("agentsandbox mount ./alpha")
              machine.succeed("agentsandbox mount ./beta sandbox-beta")
              machine.succeed("grep -F \"$(realpath alpha)\talpha\" .agentsandbox/mounts >/dev/null")
              machine.succeed("grep -F \"$(realpath beta)\tsandbox-beta\" .agentsandbox/mounts >/dev/null")
              machine.succeed("agentsandbox mount >/dev/null")
              machine.succeed("agentsandbox unmount ./alpha")
              machine.succeed("! grep -F \"$(realpath alpha)\talpha\" .agentsandbox/mounts >/dev/null")
              machine.succeed("agentsandbox build")
              machine.succeed(
                  "eval \"$(agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" agenthouse)\" && "
                  + "[[ -d \"$DATA_DIR/sysroot\" ]]"
              )
              machine.succeed(
                  "eval \"$(agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" agenthouse)\" && "
                  + "[[ -d \"$DATA_DIR/sysroot/nix/store\" ]]"
              )
              machine.succeed(
                  "eval \"$(agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" agenthouse)\" && "
                  + "machine_id_first=\"$(cat \"$DATA_DIR/machine-id\")\" && "
                  + "agentsandbox build && "
                  + "machine_id_second=\"$(cat \"$DATA_DIR/machine-id\")\" && "
                  + "[[ \"$machine_id_first\" == \"$machine_id_second\" ]]"
              )
              machine.succeed("agentsandbox up")
              machine.succeed("agentsandbox ps | grep -F running >/dev/null")
              machine.succeed("agentsandbox port >/dev/null")
              machine.succeed("agentsandbox port 22 tcp | grep -E '^[0-9]+$' >/dev/null")
              machine.succeed("agentsandbox exec -- uname -a >/dev/null")
              machine.succeed("agentsandbox logs >/dev/null")
              machine.succeed("agentsandbox stats | grep -F 'CPU %' >/dev/null")
              machine.succeed(
                  "eval \"$(agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" agenthouse)\" && "
                  + "mkdir -p \"$STATE_DIR/logs\" && "
                  + "printf '%s\\n' '{\"time\":\"2026-04-15T00:00:00Z\",\"host\":\"old.example\"}' > "
                  + "\"$STATE_DIR/logs/requests-20260415T000000Z.jsonl\" && "
                  + "zstd -fq \"$STATE_DIR/logs/requests-20260415T000000Z.jsonl\" && "
                  + "printf '%s\\n' '{\"time\":\"2026-04-15T00:00:01Z\",\"host\":\"new.example\"}' > "
                  + "\"$STATE_DIR/logs/requests.jsonl\""
              )
              machine.succeed(
                  "bash -c 'set +e; timeout 2 agentsandbox proxy-logs > /tmp/proxy-logs.out; "
                  + "st=$?; set -e; [[ $st == 124 ]]; "
                  + "old=$(grep -n \"\\\"host\\\":\\\"old.example\\\"\" /tmp/proxy-logs.out | head -n1 | cut -d: -f1); "
                  + "new=$(grep -n \"\\\"host\\\":\\\"new.example\\\"\" /tmp/proxy-logs.out | head -n1 | cut -d: -f1); "
                  + "[[ \"$old\" -lt \"$new\" ]]'"
              )
              machine.succeed("agentsandbox pause")
              machine.succeed("agentsandbox ps | grep -F paused >/dev/null")
              machine.succeed("agentsandbox unpause")
              machine.succeed("agentsandbox ps | grep -F running >/dev/null")
              machine.succeed(
                  "bash -c '"
                  + "rm -f /tmp/agsb-wait.done; "
                  + "( agentsandbox wait; touch /tmp/agsb-wait.done ) & "
                  + "sleep 2; "
                  + "agentsandbox down || true; "
                  + "for _ in $(seq 1 45); do [[ -f /tmp/agsb-wait.done ]] && break; sleep 1; done; "
                  + "[[ -f /tmp/agsb-wait.done ]]'"
              )
              machine.succeed("agentsandbox up")
              machine.succeed("agentsandbox kill")
              machine.succeed("agentsandbox destroy")
              machine.succeed(
                  "eval \"$(agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" agenthouse)\" && "
                  + "[[ ! -d \"$DATA_DIR/sysroot\" ]] && [[ -d \"$DATA_DIR/persistent\" ]]"
              )
              machine.succeed("agentsandbox wait")
              machine.succeed("agentsandbox port 50052 tcp || true")
            '';
          };
        });
    };
}
