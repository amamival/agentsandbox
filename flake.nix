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
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfree = true;
      };
      lib = nixpkgs.lib;
      templateSpec = import ./template/flake.nix;
      templateOutputs = templateSpec.outputs {
        self = { };
        inherit nixpkgs nixpkgs-unstable home-manager impermanence;
      };
      templateProfile = templateOutputs.sandboxConfigurations.default;
      templateSystem = templateOutputs.nixosConfigurations.default;
      templateXml = templateProfile.libvirtXml {
        inherit pkgs;
        name = "agentsandbox-check";
        uuid = "01234567-89ab-cdef-0123-456789abcdef";
        machineId = "0123456789abcdef0123456789abcdef";
        toplevel = "/tmp/agentsandbox-system";
        kernelParams = "console=ttyS0,115200n8 init=/tmp/agentsandbox-system/init";
        sysrootNixDir = "/tmp/sysroot/nix";
        persistentDir = "/tmp/persistent";
        runtimeDir = "/run/user/1000/agentsandbox/check";
        memoryMiB = templateProfile.memoryMiB;
        vcpus = templateProfile.vcpus;
        portForwards = templateProfile.portForwards;
      };
      nixDockerTar = pkgs.dockerTools.examples.nix;
      registryProfileLayer = pkgs.runCommand "agsb-registry-profile-layer.tar.gz" {} ''
        mkdir -p root/nix/var/nix/profiles
        ln -sfn ${pkgs.nix} root/nix/var/nix/profiles/default
        ( cd root && ${pkgs.gnutar}/bin/tar -cf - . ) | ${pkgs.gzip}/bin/gzip -9n >"$out"
      '';
      registryManifestJq = pkgs.writeText "agsb-registry-manifest.jq" ''
        {
          schemaVersion: 2,
          mediaType: "application/vnd.docker.distribution.manifest.v2+json",
          config: {
            mediaType: "application/vnd.docker.container.image.v1+json",
            size: $cs,
            digest: $d
          },
          layers: $layers
        }
      '';
      registryListJq = pkgs.writeText "agsb-registry-list.jq" ''
        {
          schemaVersion: 2,
          mediaType: "application/vnd.docker.distribution.manifest.list.v2+json",
          manifests: [
            {
              mediaType: "application/vnd.docker.distribution.manifest.v2+json",
              size: $ms,
              digest: $md,
              platform: { architecture: "amd64", os: "linux" }
            }
          ]
        }
      '';
      registryDockerhubInit = pkgs.writeShellScript "agsb-registry-dockerhub-init" ''
        set -euo pipefail
        mock=/var/lib/dockerhub-mock
        mkdir -p "$mock/www/v2/nixos/nix/manifests" "$mock/www/v2/nixos/nix/blobs"
        openssl req -x509 -newkey rsa:2048 -nodes -days 7 -keyout "$mock/ca.key" -out "$mock/ca.pem" \
          -subj "/CN=agentsandbox-dockerhub-ca"
        openssl req -newkey rsa:2048 -nodes -keyout "$mock/server.key" -out "$mock/server.csr" \
          -subj "/CN=auth.docker.io"
        printf '%s\n' '[v3_req]' 'subjectAltName=DNS:auth.docker.io,DNS:registry-1.docker.io' \
          'keyUsage=digitalSignature,keyEncipherment' 'extendedKeyUsage=serverAuth' > "$mock/server.ext"
        openssl x509 -req -days 7 -in "$mock/server.csr" -CA "$mock/ca.pem" -CAkey "$mock/ca.key" \
          -CAcreateserial -out "$mock/server.crt" -extensions v3_req -extfile "$mock/server.ext"
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        tar xf ${nixDockerTar} -C "$tmp"
        man="$tmp/manifest.json"
        cfg_rel=$(jq -r '.[0].Config' "$man")
        cfg_path="$tmp/$cfg_rel"
        cfg_d=$(sha256sum "$cfg_path" | awk '{print "sha256:" $1}')
        cfg_s=$(stat -c %s "$cfg_path")
        cp -a "$cfg_path" "$mock/www/v2/nixos/nix/blobs/$cfg_d"
        ly='[]'
        while IFS= read -r rel; do
          [[ -n "$rel" ]] || continue
          layer_path="$tmp/$rel"
          d=$(sha256sum "$layer_path" | awk '{print "sha256:" $1}')
          s=$(stat -c %s "$layer_path")
          cp -a "$layer_path" "$mock/www/v2/nixos/nix/blobs/$d"
          ly=$(jq -n --argjson L "$ly" --arg d "$d" --argjson s "$s" \
            '$L + [{mediaType:"application/vnd.docker.image.rootfs.diff.tar.gzip",digest:$d,size:$s}]')
        done < <(jq -r '.[0].Layers[]' "$man")
        prof_tar=${registryProfileLayer}
        pd=$(sha256sum "$prof_tar" | awk '{print "sha256:" $1}')
        ps=$(stat -c %s "$prof_tar")
        cp -a "$prof_tar" "$mock/www/v2/nixos/nix/blobs/$pd"
        ly=$(jq -n --argjson L "$ly" --arg d "$pd" --argjson s "$ps" \
          '$L + [{mediaType:"application/vnd.docker.image.rootfs.diff.tar.gzip",digest:$d,size:$s}]')
        jq -n --arg d "$cfg_d" --argjson cs "$cfg_s" --argjson layers "$ly" -f ${registryManifestJq} \
          > "$mock/manifest.json"
        manifest_size=$(stat -c %s "$mock/manifest.json")
        manifest_digest=sha256:$(sha256sum "$mock/manifest.json" | awk '{print $1}')
        cp -a "$mock/manifest.json" "$mock/www/v2/nixos/nix/manifests/$manifest_digest"
        jq -n --argjson ms "$manifest_size" --arg md "$manifest_digest" -f ${registryListJq} \
          > "$mock/www/v2/nixos/nix/manifests/latest"
        chmod -R a+rX "$mock"
      '';
    in
    {
      packages.${system}.default = pkgs.stdenvNoCC.mkDerivation {
        pname = "agentsandbox";
        version = "0.0.0-dev";
        src = self;
        dontUnpack = true;
        installPhase = ''
          install -Dm755 ${./agentsandbox} "$out/bin/agentsandbox"
          install -Dm644 ${./template/flake.nix} "$out/share/agentsandbox/template/flake.nix"
          install -Dm644 ${./template/configuration.nix} "$out/share/agentsandbox/template/configuration.nix"
          install -Dm644 ${./template/allowed_hosts} "$out/share/agentsandbox/template/allowed_hosts"
          install -Dm644 ${./template/mounts} "$out/share/agentsandbox/template/mounts"
        '';
      };

      apps.${system}.default = {
        type = "app";
        program = "${self.packages.${system}.default}/bin/agentsandbox";
        meta.description = "AgentSandbox shell launcher";
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          bash
          bubblewrap
          coreutils
          curl
          gawk
          gnugrep
          jq
          libvirt
          mitmproxy
          openssh
          passt
          qemu_kvm
          shellcheck
          socat
          util-linux
          virtiofsd
          zstd
        ];
        LIBVIRT_DEFAULT_URI = "qemu:///session";
      };

      checks.${system} = {
        lint = pkgs.runCommand "lint" {} ''
          ${pkgs.shellcheck}/bin/shellcheck ${./agentsandbox} ${./tests.sh} > "$out"
        '';

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
            virtualisation.additionalPaths = [
              (toString nixpkgs)
              (toString home-manager)
              (toString impermanence)
            ];
            virtualisation.diskSize = 65536;
            virtualisation.memorySize = 12288;
            virtualisation.cores = 6;
            networking.interfaces.eth1 = {
              useDHCP = false;
              ipv4.addresses = [
                { address = "192.168.1.1"; prefixLength = 24; }
              ];
            };
            networking.hosts."192.168.1.2" = [
              "auth.docker.io"
              "registry-1.docker.io"
            ];
            nix.settings.experimental-features = [ "nix-command" "flakes" ];
            environment.systemPackages = with pkgs; [
              bash
              bubblewrap
              curl
              jq
              libvirt
              openssh
              passt
              qemu_kvm
              socat
              util-linux
              virtiofsd
              zstd
              nix
              git
              coreutils
              gawk
              gnugrep
              gnused
              findutils
              mitmproxy
            ];
            virtualisation.libvirtd.enable = true;
          };
          nodes.registry = { pkgs, ... }: {
            networking.firewall.allowedTCPPorts = [ 443 ];
            networking.interfaces.eth1 = {
              useDHCP = false;
              ipv4.addresses = [
                { address = "192.168.1.2"; prefixLength = 24; }
              ];
            };
            environment.systemPackages = with pkgs; [ curl gnugrep nix openssl ];
            networking.hosts."127.0.0.1" = [
              "auth.docker.io"
              "registry-1.docker.io"
            ];
            nix.settings.experimental-features = [ "nix-command" ];
            systemd.services.dockerhub-mock-init = {
              wantedBy = [ "multi-user.target" ];
              before = [ "nginx.service" ];
              requiredBy = [ "nginx.service" ];
              after = [ "nix-daemon.socket" ];
              path = with pkgs; [
                bash
                coreutils
                gawk
                gzip
                gnugrep
                gnused
                gnutar
                jq
                openssl
              ];
              serviceConfig = {
                Type = "oneshot";
                RemainAfterExit = true;
              };
              script = "${registryDockerhubInit}";
            };
            services.nginx.enable = true;
            services.nginx.virtualHosts."auth.docker.io" = {
              onlySSL = true;
              sslCertificate = "/var/lib/dockerhub-mock/server.crt";
              sslCertificateKey = "/var/lib/dockerhub-mock/server.key";
              extraConfig = ''
                location = /token {
                  default_type application/json;
                  return 200 '{"token":"dummy-token"}';
                }
              '';
            };
            services.nginx.virtualHosts."registry-1.docker.io" = {
              onlySSL = true;
              sslCertificate = "/var/lib/dockerhub-mock/server.crt";
              sslCertificateKey = "/var/lib/dockerhub-mock/server.key";
              root = "/var/lib/dockerhub-mock/www";
            };
          };
          testScript = ''
            start_all()
            machine.wait_for_unit("multi-user.target")
            registry.wait_for_unit("multi-user.target")
            registry.wait_for_unit("dockerhub-mock-init.service")
            registry.wait_for_unit("nginx.service")
            machine.wait_for_unit("libvirtd.service")
            registry.succeed(
                "curl -fsS --cacert /var/lib/dockerhub-mock/ca.pem https://auth.docker.io/token | grep -q dummy-token"
            )
            registry.succeed(
                "curl -fsS --cacert /var/lib/dockerhub-mock/ca.pem https://registry-1.docker.io/v2/nixos/nix/manifests/latest | grep -q manifest"
            )
            machine.succeed("mkdir -p /root/work /root/e2e")
            registry_ca_b64 = registry.succeed("base64 -w0 /var/lib/dockerhub-mock/ca.pem").strip()
            machine.succeed(f"printf '%s' '{registry_ca_b64}' | base64 -d > /root/e2e/registry-ca.pem")
            machine.succeed("cp -a ${self}/. /root/work/")
            machine.succeed("chmod +x /root/work/agentsandbox")
            machine.succeed(
                "mkdir -p /root/e2e/home /root/e2e/config /root/e2e/data /root/e2e/state /root/e2e/runtime"
            )
            machine.succeed("mkdir -p /root/e2e/project")
            machine.succeed("cp -a /root/work/agentsandbox /root/e2e/project/agentsandbox")
            machine.succeed("cp -a /root/work/template /root/e2e/project/template")
            machine.succeed("cp -a /root/work/flake.lock /root/e2e/project/flake.lock")
            machine.succeed("chmod +x /root/e2e/project/agentsandbox")
            agsb = (
                "export HOME=/root/e2e/home "
                "XDG_CONFIG_HOME=/root/e2e/config "
                "XDG_DATA_HOME=/root/e2e/data "
                "XDG_STATE_HOME=/root/e2e/state "
                "XDG_RUNTIME_DIR=/root/e2e/runtime "
                "CURL_CA_BUNDLE=/root/e2e/registry-ca.pem && "
                "cd /root/e2e/project && "
            )
            machine.succeed(agsb + "test -f /root/e2e/registry-ca.pem")
            machine.succeed(agsb + "./agentsandbox init")
            machine.succeed(agsb + "cp -a /root/e2e/project/flake.lock .agentsandbox/flake.lock")
            machine.succeed(agsb + "mkdir -p .agentsandbox/_pin")
            machine.succeed(
                agsb
                + "cp -a ${toString nixpkgs} .agentsandbox/_pin/nixpkgs && "
                + "cp -a ${toString home-manager} .agentsandbox/_pin/home-manager && "
                + "cp -a ${toString impermanence} .agentsandbox/_pin/impermanence"
            )
            machine.succeed(
                agsb
                + "sed -i "
                + "-e 's|github:NixOS/nixpkgs/nixos-25.11|path:./_pin/nixpkgs|' "
                + "-e 's|github:nix-community/home-manager/release-25.11|path:./_pin/home-manager|' "
                + "-e 's|github:nix-community/impermanence|path:./_pin/impermanence|' "
                + ".agentsandbox/flake.nix"
            )
            machine.succeed(agsb + "./agentsandbox help | grep -F proxy-logs >/dev/null")
            machine.succeed(agsb + "./agentsandbox version | grep -E '^[^[:space:]]+$' >/dev/null")
            machine.succeed(agsb + "./agentsandbox doctor | grep -F uri: >/dev/null")
            machine.succeed(
                agsb
                + "eval \"$(./agentsandbox __resolve-active-config \"$PWD\")\" && "
                + "[[ \"$ACTIVE_CONFIG_DIR\" == \"$PWD/.agentsandbox\" ]]"
            )
            machine.succeed(
                agsb
                + "eval \"$(./agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" default)\" && "
                + "[[ \"$INSTANCE_ID\" == \"''${INSTANCE_NAME}-''${MACHINE_ID}\" ]]"
            )
            machine.succeed(agsb + "./agentsandbox allow-domain Example.COM")
            machine.succeed(agsb + "./agentsandbox allow-domain https://example.com/path")
            machine.succeed(agsb + "./agentsandbox allow-domain 'https://*.Example.COM.:8443/path'")
            machine.succeed(agsb + "[[ \"$(grep -Fxc example.com .agentsandbox/allowed_hosts)\" == 1 ]]")
            machine.succeed(agsb + "grep -Fx '*.example.com' .agentsandbox/allowed_hosts >/dev/null")
            machine.succeed(agsb + "./agentsandbox unallow-domain https://EXAMPLE.com.:443/path")
            machine.succeed(agsb + "! grep -Fx example.com .agentsandbox/allowed_hosts >/dev/null")
            machine.succeed(agsb + "mkdir -p alpha beta")
            machine.succeed(agsb + "./agentsandbox mount ./alpha")
            machine.succeed(agsb + "./agentsandbox mount ./beta sandbox-beta")
            machine.succeed(agsb + "grep -F \"$(realpath alpha)\talpha\" .agentsandbox/mounts >/dev/null")
            machine.succeed(agsb + "grep -F \"$(realpath beta)\tsandbox-beta\" .agentsandbox/mounts >/dev/null")
            machine.succeed(agsb + "./agentsandbox mount >/dev/null")
            machine.succeed(agsb + "./agentsandbox unmount ./alpha")
            machine.succeed(agsb + "! grep -F \"$(realpath alpha)\talpha\" .agentsandbox/mounts >/dev/null")
            machine.succeed(agsb + "./agentsandbox build")
            machine.succeed(agsb + "eval \"$(./agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" default)\" && [[ -d \"$DATA_DIR/sysroot\" ]]")
            machine.succeed(agsb + "eval \"$(./agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" default)\" && [[ -d \"$DATA_DIR/sysroot/nix/store\" ]]")
            machine.succeed(
                agsb
                + "eval \"$(./agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" default)\" && "
                + "machine_id_first=\"$(cat \"$DATA_DIR/machine-id\")\" && "
                + "./agentsandbox build && "
                + "machine_id_second=\"$(cat \"$DATA_DIR/machine-id\")\" && "
                + "[[ \"$machine_id_first\" == \"$machine_id_second\" ]]"
            )
            machine.succeed(agsb + "./agentsandbox up")
            machine.succeed(agsb + "./agentsandbox ps | grep -F running >/dev/null")
            machine.succeed(agsb + "./agentsandbox port >/dev/null")
            machine.succeed(agsb + "./agentsandbox port 22 tcp | grep -E '^[0-9]+$' >/dev/null")
            machine.succeed(agsb + "./agentsandbox exec -- uname -a >/dev/null")
            machine.succeed(agsb + "./agentsandbox logs >/dev/null")
            machine.succeed(agsb + "./agentsandbox stats | grep -F 'CPU %' >/dev/null")
            machine.succeed(
                agsb
                + "eval \"$(./agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" default)\" && "
                + "[[ -d \"$DATA_DIR/persistent/home/vscode\" ]] && "
                + "[[ -d \"$DATA_DIR/persistent/workspace\" ]] && "
                + "[[ -d \"$DATA_DIR/persistent/workspace/sandbox-beta\" ]]"
            )
            machine.succeed(
                agsb
                + "eval \"$(./agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" default)\" && "
                + "mkdir -p \"$STATE_DIR/logs\" && "
                + "printf '%s\\n' '{\"time\":\"2026-04-15T00:00:00Z\",\"host\":\"old.example\"}' > "
                + "\"$STATE_DIR/logs/requests-20260415T000000Z.jsonl\" && "
                + "zstd -fq \"$STATE_DIR/logs/requests-20260415T000000Z.jsonl\" && "
                + "printf '%s\\n' '{\"time\":\"2026-04-15T00:00:01Z\",\"host\":\"new.example\"}' > "
                + "\"$STATE_DIR/logs/requests.jsonl\""
            )
            machine.succeed(
                agsb
                + "bash -c 'set +e; timeout 2 ./agentsandbox proxy-logs > /tmp/proxy-logs.out; "
                + "st=$?; set -e; [[ $st == 124 ]]; "
                + "old=$(grep -n \"\\\"host\\\":\\\"old.example\\\"\" /tmp/proxy-logs.out | head -n1 | cut -d: -f1); "
                + "new=$(grep -n \"\\\"host\\\":\\\"new.example\\\"\" /tmp/proxy-logs.out | head -n1 | cut -d: -f1); "
                + "[[ \"$old\" -lt \"$new\" ]]'"
            )
            machine.succeed(agsb + "./agentsandbox pause")
            machine.succeed(agsb + "./agentsandbox ps | grep -F paused >/dev/null")
            machine.succeed(agsb + "./agentsandbox unpause")
            machine.succeed(agsb + "./agentsandbox ps | grep -F running >/dev/null")
            machine.succeed(
                agsb
                + "bash -c '"
                + "rm -f /tmp/agsb-wait.done; "
                + "( ./agentsandbox wait; touch /tmp/agsb-wait.done ) & "
                + "sleep 2; "
                + "./agentsandbox down || true; "
                + "for _ in $(seq 1 45); do [[ -f /tmp/agsb-wait.done ]] && break; sleep 1; done; "
                + "[[ -f /tmp/agsb-wait.done ]]'"
            )
            machine.succeed(agsb + "./agentsandbox up")
            machine.succeed(agsb + "./agentsandbox kill")
            machine.succeed(agsb + "./agentsandbox destroy")
            machine.succeed(
                agsb
                + "eval \"$(./agentsandbox __resolve-instance \"$PWD\" \"$PWD/.agentsandbox\" default)\" && "
                + "[[ ! -d \"$DATA_DIR/sysroot\" ]] && [[ -d \"$DATA_DIR/persistent\" ]]"
            )
            machine.succeed(agsb + "./agentsandbox wait")
            machine.succeed(agsb + "./agentsandbox port 50052 tcp || true")
          '';
        };
      };
    };
}
