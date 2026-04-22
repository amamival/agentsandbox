{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url = "github:nix-community/home-manager/release-25.11";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";
    impermanence.url = "github:nix-community/impermanence";
    impermanence.inputs.nixpkgs.follows = "nixpkgs";
    impermanence.inputs.home-manager.follows = "home-manager";
  };
  outputs = { self, nixpkgs, nixpkgs-unstable, home-manager, impermanence, ... }:
    let
      eachSystem = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
      pkgsFor = system: import nixpkgs { inherit system; config.allowUnfree = true; };
    in
    {
      packages = eachSystem
        (system:
          let
            pkgs = pkgsFor system;
            agentsandbox-rs = pkgs.rustPlatform.buildRustPackage {
              pname = "agentsandbox_rs";
              version = "0.0.0";
              src = ./.;
              cargoLock.lockFile = ./Cargo.lock;
            };
          in {
            default = pkgs.stdenvNoCC.mkDerivation {
              pname = "agentsandbox";
              version = "0.0.0-dev";
              src = self;
              dontUnpack = true;
              nativeBuildInputs = [ pkgs.makeWrapper ];
              installPhase = ''
                mkdir "$out" && cp -r "$src"/{bin,share} "$out/" && chmod -R u+w "$out"
                install -Dm755 ${agentsandbox-rs}/bin/agentsandbox_rs "$out/bin/agentsandbox_rs"
                wrapProgram "$out/bin/agentsandbox" \
                  --prefix PATH : ${pkgs.lib.makeBinPath (with pkgs; [
                    bubblewrap curl jq libvirt mitmproxy openssh passt socat util-linux virtiofsd zstd
                  ])} 
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
            packages = with pkgs; [ bubblewrap cargo clippy curl jq libvirt mitmproxy openssh passt qemu_kvm rust-analyzer rustc rustfmt socat util-linux virtiofsd zstd ];
            LIBVIRT_DEFAULT_URI = "qemu:///session";
          };
        });

      checks = eachSystem (system:
        let
          pkgs = pkgsFor system;
          testNixpkgs = let outPath = nixpkgs.outPath; in {
            inherit outPath;
            lib = (import (outPath + "/lib")).extend
              ((import (outPath + "/lib/flake-version-info.nix")) { inherit outPath; });
          };
          testProfileName = "default";
          testProjectName = "tmp";
          testMachinePrefix = "000000000000000000000000";
          testInstanceMachineId =
            testMachinePrefix + builtins.substring 0 8 (builtins.hashString "sha256" testProfileName);
          testInstanceId = "${testProjectName}-${testProfileName}-${testInstanceMachineId}";
          testShortMachineId = builtins.substring (builtins.stringLength testInstanceId - 20) 20 testInstanceId;
          testDomainUuid =
            "${builtins.substring 0 8 testShortMachineId}-${builtins.substring 8 4 testShortMachineId}"
            + "-${builtins.substring 12 4 testShortMachineId}-${builtins.substring 16 4 testShortMachineId}"
            + "-${builtins.substring 20 12 testShortMachineId}";
          testDataDir = "/root/.local/share/agentsandbox/${testInstanceId}";
          testRuntimeDir = "/run/user/0/agentsandbox/${testInstanceId}";
          testGuestConfig = (
            import (nixpkgs.outPath + "/nixos/lib/eval-config.nix") ({
              lib = testNixpkgs.lib;
              system = null;
              modules = [
                ./share/agentsandbox/template/configuration.nix
                home-manager.nixosModules.home-manager
                ((import ./share/agentsandbox/template/agentsandbox/flake.nix).outputs {
                  self = builtins.getFlake (toString ./share/agentsandbox/template/agentsandbox);
                  inherit nixpkgs impermanence;
                }).nixosModules.default
                (_: { nixpkgs.overlays = [ (_: _: { }) ]; })
                ({ ... }: { config.nixpkgs.flake.source = testNixpkgs.outPath; })
              ];
              specialArgs = {
                pkgs-unstable = import nixpkgs-unstable { inherit system; config.allowUnfree = true; };
                nixpkgs = testNixpkgs;
              };
            } // { inherit system; })
          ).config;
          testGuestToplevel = testGuestConfig.system.build.toplevel;
          testLibvirtXml = pkgs.runCommand "${testInstanceId}.xml" {
            DOMAIN_UUID = testDomainUuid;
            GID_MAP = ''
                     100        100          1
                       0     100000        100
                     101     100100     165435
            '';
            INSTANCE_ID = testInstanceId;
            MACHINE_ID = testShortMachineId;
            NIX_DIR = "${testDataDir}/sysroot/nix";
            PERSISTENT_DIR = "${testDataDir}/persistent";
            RUNTIME_DIR = testRuntimeDir;
            UID_MAP = ''
                      1000       1000          1
                         0     100000       1000
                      1001     101000     164535
            '';
          } ''
            "${testGuestToplevel}/domain.xml.sh" > "$out"
          '';
          rootLock = builtins.fromJSON (builtins.readFile ./flake.lock);
          impermanenceLock = builtins.fromJSON (builtins.readFile "${impermanence.outPath}/flake.lock");
          homeManagerPathLock = {
            lastModified = 0;
            narHash = rootLock.nodes.home-manager.locked.narHash;
            path = home-manager.outPath;
            type = "path";
          };
          impermanencePathLock = {
            lastModified = 0;
            narHash = rootLock.nodes.impermanence.locked.narHash;
            path = impermanence.outPath;
            type = "path";
          };
          nixpkgsPathLock = {
            lastModified = 0;
            narHash = rootLock.nodes.nixpkgs.locked.narHash;
            path = nixpkgs.outPath;
            type = "path";
          };
          nixpkgsUnstablePathLock = {
            lastModified = 0;
            narHash = rootLock.nodes."nixpkgs-unstable".locked.narHash;
            path = nixpkgs-unstable.outPath;
            type = "path";
          };
          testFlakeLock = pkgs.writeText "agentsandbox-test-flake.lock" (builtins.toJSON {
            version = 7;
            root = "root";
            nodes = {
              agentsandbox = {
                inputs = {
                  impermanence = "impermanence";
                  nixpkgs = "nixpkgs_2";
                };
                locked = { path = "./agentsandbox"; type = "path"; };
                original = { path = "./agentsandbox"; type = "path"; };
                parent = [ ];
              };
              home-manager = {
                inputs.nixpkgs = [ "agentsandbox" "impermanence" "nixpkgs" ];
                locked = homeManagerPathLock;
                original = impermanenceLock.nodes.home-manager.original;
              };
              home-manager_2 = {
                inputs.nixpkgs = [ "nixpkgs" ];
                locked = homeManagerPathLock;
                original = rootLock.nodes.home-manager.original;
              };
              impermanence = {
                inputs = {
                  home-manager = "home-manager";
                  nixpkgs = "nixpkgs";
                };
                locked = impermanencePathLock;
                original = rootLock.nodes.impermanence.original;
              };
              nixpkgs = {
                locked = nixpkgsPathLock;
                original = impermanenceLock.nodes.nixpkgs.original;
              };
              nixpkgs_2 = {
                locked = nixpkgsPathLock;
                original = rootLock.nodes.nixpkgs.original;
              };
              nixpkgs-unstable = {
                locked = nixpkgsUnstablePathLock;
                original = rootLock.nodes."nixpkgs-unstable".original;
              };
              root.inputs = {
                agentsandbox = "agentsandbox";
                home-manager = "home-manager_2";
                nixpkgs = "nixpkgs_2";
                nixpkgs-unstable = "nixpkgs-unstable";
              };
            };
          });
        in {
          lint = pkgs.runCommand "lint" { } ''${pkgs.shellcheck}/bin/shellcheck ${./bin}/* > "$out"'';
          nixos-e2e = pkgs.testers.runNixOSTest {
            name = "agentsandbox-nixos-e2e";
            nodes.machine = { pkgs, ... }: {
              virtualisation.cores = 8;
              virtualisation.memorySize = 8 * 1024;
              virtualisation.diskSize = 16 * 1024;
              virtualisation.additionalPaths = [
                testFlakeLock
                testLibvirtXml
                home-manager.outPath
                nixpkgs.outPath
                nixpkgs-unstable.outPath
                impermanence.outPath
              ];

              security.pki.certificateFiles = [ share/agentsandbox/mitm-proxy-ca.pem ];
              networking.interfaces.eth1 = {
                useDHCP = false;
                ipv4.addresses = [{ address = "192.168.1.1"; prefixLength = 24; }];
              };
              networking.hosts."192.168.1.2" = [ "auth.docker.io" "registry-1.docker.io" ];

              virtualisation.libvirtd.enable = true;
              users.users.root = {
                subUidRanges = [{ startUid = 100000; count = 65536; }];
                subGidRanges = [{ startGid = 100000; count = 65536; }];
              };

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
                sslCertificate = share/agentsandbox/mitm-proxy-ca.pem;
                sslCertificateKey = share/agentsandbox/mitm-proxy-ca.key;
                locations."= /token".return = ''200 '{"token":"test"}' '';
              };
              services.nginx.virtualHosts."registry-1.docker.io" = {
                onlySSL = true;
                sslCertificate = share/agentsandbox/mitm-proxy-ca.pem;
                sslCertificateKey = share/agentsandbox/mitm-proxy-ca.key;
                locations."= /v2/nixos/nix/blobs/0".alias = pkgs.runCommand "docker-image-nix-layer" { } ''
                  tar xf ${pkgs.dockerTools.examples.nix} --wildcards '*/layer.tar' -O >layer.tar
                  mkdir -p work/nix/var/nix/profiles work/etc/nixos
                  ln -sfn / work/nix/var/nix/profiles/default
                  printf 'root:x:0:0::/root:/bin/sh\nnixbld1:x:30001:30000::/:/bin/sh' >work/etc/passwd
                  printf 'root:x:0:\nnixbld:x:30000:nixbld1' >work/etc/group
                  (cd work && ${pkgs.gnutar}/bin/tar -rf ../layer.tar .)
                  ${pkgs.gzip}/bin/gzip -9n <layer.tar >"$out"
                '';
                locations."= /v2/nixos/nix/manifests/latest".return =
                  ''200 '{"manifests":[{"digest":"main","platform":{"architecture":"amd64","os":"linux"}}]}' '';
                locations."= /v2/nixos/nix/manifests/main".return = ''200 '{"layers":[{"digest":"0"}]}' '';
              };
            };
            testScript = ''
              label = "25.11.19700101.dirty"

              start_all()
              registry.wait_for_unit("nginx.service")
              machine.succeed("curl -fsS https://auth.docker.io/token | grep -q test")
              machine.succeed("curl -fsS https://registry-1.docker.io/v2/nixos/nix/manifests/latest | grep -q manifest")
              machine.succeed("curl -fsS https://registry-1.docker.io/v2/nixos/nix/manifests/main | grep -q layer")
              machine.succeed("curl -fsS --head https://registry-1.docker.io/v2/nixos/nix/blobs/0")

              machine.wait_for_unit("libvirtd.service")
              machine.succeed("su -s /bin/sh -c 'unshare --map-auto --setuid=0 --setgid=0 true' root")
              machine.succeed("install -d /run/user/0")
              machine.succeed("agentsandbox init")
              machine.succeed("printf '%s' '${testMachinePrefix}' > .agentsandbox/machine-prefix")
              workspace = machine.succeed("pwd").strip()
              workspace_name = machine.succeed('basename "$PWD"').strip()
              flake_dir = machine.succeed('agentsandbox __get_or_create_flake_dir "$PWD"').strip()
              instance_id = machine.succeed(
                  'agentsandbox __get_or_create_instance_id "$PWD/.agentsandbox" default'
              ).strip()
              machine_id = instance_id.rsplit("-", 1)[-1]
              data_dir = f"/root/.local/share/agentsandbox/{instance_id}"
              state_dir = f"/root/.local/state/agentsandbox/{instance_id}"
              runtime_dir = f"/run/user/0/agentsandbox/{instance_id}"

              assert flake_dir == f"{workspace}/.agentsandbox"
              machine.succeed("cp ${testFlakeLock} .agentsandbox/flake.lock")
              machine.succeed(
                  "nix-store --export $(nix-store -qR '${testGuestToplevel}') "
                  + "${home-manager.outPath} ${nixpkgs.outPath} "
                  + "${nixpkgs-unstable.outPath} ${impermanence.outPath} "
                  + "> /tmp/agentsandbox-guest.nar"
              )
              machine.succeed("agentsandbox help | grep -F verify >/dev/null")
              machine.succeed("agentsandbox version | grep -E '^[^[:space:]]+$' >/dev/null")
              doctor = machine.succeed("agentsandbox doctor")
              assert "libvirt-uri: qemu:///session" in doctor
              assert "profile: default" in doctor
              assert f"active-config: {flake_dir}" in doctor
              assert "config-scope: local" in doctor
              assert f"instance-id: {instance_id}" in doctor
              assert f"machine-id: {machine_id[-20:]}" in doctor
              assert f"data-dir: {data_dir}" in doctor
              assert f"state-dir: {state_dir}" in doctor
              assert f"runtime-dir: {runtime_dir}" in doctor
              for key in [
                  "nix:",
                  "virsh:",
                  "qemu:",
                  "virtiofsd:",
                  "mitmdump:",
                  "bwrap:",
                  "jq:",
                  "curl:",
                  "zstd:",
                  "socat:",
              ]:
                  assert key in doctor
              machine.succeed('grep -Fx "# <host-path><TAB><guest-name>" .agentsandbox/mounts >/dev/null')
              machine.succeed(f'grep -F "{workspace}\t{workspace_name}" .agentsandbox/mounts >/dev/null')
              machine.succeed("agentsandbox verify | tee /tmp/agentsandbox-verify.out >/dev/null")
              machine.succeed("grep -F 'nixos-rebuild --repair' /tmp/agentsandbox-verify.out >/dev/null")
              machine.succeed("grep -F 'nix store verify --repair' /tmp/agentsandbox-verify.out >/dev/null")
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
              machine.succeed("agentsandbox mount | grep -F sandbox-beta >/dev/null")
              machine.succeed("agentsandbox unmount ./alpha")
              machine.succeed("! grep -F \"$(realpath alpha)\talpha\" .agentsandbox/mounts >/dev/null")
              machine.succeed(
                  f"mkdir -p '{data_dir}/sysroot' && "
                  + "curl -fsSI -H 'Authorization: Bearer test' "
                  + "https://registry-1.docker.io/v2/nixos/nix/blobs/0 >/dev/null && "
                  + "curl -fsSL -H 'Authorization: Bearer test' "
                  + "https://registry-1.docker.io/v2/nixos/nix/blobs/0 | "
                  + f"unshare --map-auto --map-root-user --wd '{data_dir}/sysroot' tar zxf - && "
                  + f"unshare --map-auto --map-root-user --wd '{data_dir}/sysroot' "
                  + f"nix-store --store '{data_dir}/sysroot' --import < /tmp/agentsandbox-guest.nar"
              )
              machine.succeed(f"NIXOS_LABEL={label} agentsandbox build")
              machine.succeed(f"[[ -d '{data_dir}/sysroot' ]]")
              machine.succeed(f"[[ -L '{data_dir}/sysroot/nix/var/nix/profiles/system' ]]")
              machine.succeed(f"[[ -d '{data_dir}/sysroot/nix/store' ]]")
              machine.succeed(f"NIXOS_LABEL={label} agentsandbox build")
              assert instance_id == machine.succeed(
                  'agentsandbox __get_or_create_instance_id "$PWD/.agentsandbox" default'
              ).strip()
              machine.succeed(f"NIXOS_LABEL={label} agentsandbox up")
              machine.succeed(
                  f"[[ -f '{state_dir}/logs/runtime.log' && -f '{state_dir}/logs/requests.jsonl' ]] && "
                  + f"[[ -S '{runtime_dir}/virtiofs/nix.sock' && -S '{runtime_dir}/virtiofs/persistent.sock' ]]"
              )
              machine.succeed("agentsandbox ps | grep -F running >/dev/null")
              default_port = machine.succeed("agentsandbox port").strip()
              ssh_port = machine.succeed("agentsandbox port 22 tcp").strip()
              assert default_port == ssh_port
              assert ssh_port.isdigit()
              machine.succeed("agentsandbox ssh whoami | grep -Fx vscode >/dev/null")
              machine.succeed("agentsandbox exec test -d /persistent/home/vscode")
              machine.succeed(f"agentsandbox exec test -d /persistent/workspace/{workspace_name}")
              machine.succeed("agentsandbox exec test -d /persistent/workspace/sandbox-beta")
              machine.succeed("agentsandbox logs >/dev/null")
              machine.succeed("agentsandbox stats | grep -F 'CPU %' >/dev/null")
              machine.succeed(
                  f"mkdir -p '{state_dir}/logs' && "
                  + "printf '%s\\n' '{\"time\":\"2026-04-15T00:00:00Z\",\"host\":\"old.example\"}' > "
                  + f"'{state_dir}/logs/requests-20260415T000000Z.jsonl' && "
                  + f"zstd -fq --rm '{state_dir}/logs/requests-20260415T000000Z.jsonl' && "
                  + "printf '%s\\n' '{\"time\":\"2026-04-15T00:00:01Z\",\"host\":\"new.example\"}' > "
                  + f"'{state_dir}/logs/requests.jsonl'"
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
              machine.succeed(
                  f"[[ ! -e '{runtime_dir}/mount-helper.pid' && ! -e '{runtime_dir}/proxy.pid' ]] && "
                  + f"[[ ! -e '{runtime_dir}/virtiofs/nix.sock' && ! -e '{runtime_dir}/virtiofs/persistent.sock' ]]"
              )
              machine.succeed("agentsandbox wait")
              machine.succeed("agentsandbox up")
              machine.succeed(f"touch '{data_dir}/persistent/canary'")
              machine.succeed("agentsandbox kill")
              machine.succeed(
                  f"[[ ! -e '{runtime_dir}/mount-helper.pid' && ! -e '{runtime_dir}/proxy.pid' ]] && "
                  + f"[[ ! -e '{runtime_dir}/virtiofs/nix.sock' && ! -e '{runtime_dir}/virtiofs/persistent.sock' ]]"
              )
              machine.succeed("agentsandbox destroy")
              machine.succeed(
                  f"[[ -d '{data_dir}/sysroot' && -f '{data_dir}/persistent/canary' ]]"
              )
              machine.succeed("agentsandbox destroy -s")
              machine.succeed(
                  f"[[ ! -d '{data_dir}/sysroot' && -f '{data_dir}/persistent/canary' ]]"
              )
              machine.succeed("agentsandbox up")
              machine.succeed("agentsandbox exec test -f /persistent/canary")
              machine.succeed("agentsandbox destroy -sd")
              machine.succeed("[[ -z \"$(agentsandbox port 50052 tcp)\" ]]")
            '';
          };
        });
    };
}
