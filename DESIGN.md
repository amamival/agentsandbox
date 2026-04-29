# Agent Sandbox: a secure, efficient, reproducible NixOS Linux VM for self-improving agentic workflows

In 2026, agentic loops are becoming increasingly unattended as LLM-based coding harnesses improve. However, the use of such harnesses and external LLM providers still raises security and privacy concerns.

Existing tools attempt to confine a session, but bubblewrap-based containers often map the current host user to root inside the container. If the workload escapes, it can quietly read the host user's `.ssh` credentials or API secrets as that user. In addition, when we want an agent to configure a Linux system directly, we may need to expose `CAP_SYS_ADMIN` (for rootless containers with `systemd`) and/or `/dev/kvm` (for QEMU/KVM), which exposes additional attack surface.

Running coding agents in *dangerous mode* - with full access to the local machine and the internet - is desirable for productivity. However, it can expose network fingerprints such as the hostname, usernames, MAC address, and network topology, and it can allow read/write access to files outside the session workspace through supply-chain attacks. A simpler approach, such as a NixOS container that reuses the host `/nix`, still makes it easy for a workload to identify vulnerable or profitable targets on the host.

## About

This repo boots a NixOS-based system sandbox on a local Linux host.

It gives you a real booted NixOS userland with `systemd`, persistence, and SSH access.

This is a good fit for packaging, service work, NixOS modules, and NixOS learning in general. You can iterate on a real [`configuration.nix`](configuration.nix), rebuild, and observe how services, users, SSH, packages, and persistent state behave together without sacrificing security and privacy.

This is not yet a polished runtime. It remains an experimental launcher under heavy development.

The target host platform is recent `amd64` Linux in general, not just NixOS. If this does not run on a reasonably current Linux machine, that should be treated as a bug rather than an unsupported edge case.

The entrypoint is [`agentsandbox`](agentsandbox), which handles sysroot bootstrap, system build, libvirt startup, attach, and mounts.

## Installation

- Nix Flakes: `nix run github:<OWNER>/<REPO>`
- Debian/Ubuntu, Fedora/RHEL, Arch: install packages from [GitHub Releases](https://github.com/<OWNER>/<REPO>/releases)
  - `.deb` / `.rpm` / `.pkg.tar.zst`

## Using

Linux host (x86_64/amd64) with KVM support is required.

- `agentsandbox`
  If the VM is running, attach to it; otherwise, rebuild and start it.
- `agentsandbox <command>`
  Run one of the commands below against the selected workspace/config/hostname.

The commands are similar to those of **Docker Compose**.
```
Usage: agentsandbox [OPTIONS] [COMMAND]

Commands:
  version         Show version
  doctor          Show diagnostics
  init            Create `.agentsandbox/` and write the initial template files
  build           Build the guest system
  up              Rebuild and start a VM; if already running, build and switch
  down            Tear down the VM gracefully
  kill            Forcibly stop the VM
  pause           Pause running VMs for all hostnames in the current config
  unpause         Unpause VMs for all hostnames in the current config
  destroy         Kill and delete guest files selected by flags (none by default)
  ps              List VM statuses for all hostnames in the current config
  ssh             Run a command as a user in a running VM, or attach if omitted
  exec            Run a command as root in a running VM, or attach if omitted
  logs            Show logs from a running VM. Runs `journalctl` with `-en1000` by default
  stats           Display statistics of CPU time, memory for VMs
  wait            Block until this VM becomes one of the states. Wait for stop states by default
  mount           Mount a file or directory into a running VM, or show mounts entries
  unmount         Unmount a file or directory from a running VM now and on future starts
  port            Prints the public port for a port binding
  allow-domain    Add a firewall rule that allows outbound traffic to a domain
  unallow-domain  Remove the rule for the domain
  proxy-logs      Follow MITM proxy logs
  verify          Verify and repair build
  help            Print this message or the help of the given subcommand(s)

Options:
  -g, --global                 Use only global config (`$XDG_CONFIG_HOME/agentsandbox`) and skip local upward search
  -n, --hostname <HOSTNAME>    Select sandbox hostname (build target and instance identity input) [default: default]
  -w, --workspace <WORKSPACE>  Resolve the active workspace and config as if running from this directory
  -h, --help                   Print help
  -V, --version                Print version
```

## Quick Start
```bash
# 1) Initialize local config in current workspace
agentsandbox init
# 2) Build and start VM (attaches if startup succeeds)
agentsandbox up
# 3) Open guest shell (user)
agentsandbox ssh
# 4) Run command as root in guest
agentsandbox exec -- uname -a
# 5) Stop VM gracefully
agentsandbox down
```
For global (project-less) usage, initialize once with:
```bash
agentsandbox --global init
```

## Development

Nix users can run `nix develop` then `cargo run <subcommand> <options>`.\
Otherwise, install dependencies (`cargo`, `libvirt`, `virtiofsd`, `mitmproxy`, `openssh`, `util-linux`).
Use `doctor` subcommand to verify host setup.

## License

MIT


## Design notes

In our experiments, *gVisor* could not run *SystemD* as PID 1 because it lacks the `fsopen` and `rseq` syscalls.

## Similar project

[devsandbox](https://github.com/zekker6/devsandbox) - a `bwrap`-based container runtime with request logs, domain filtering, egress secret reduction, `GH_TOKEN` injection, desktop notification, and automatic sharing of tool configuration.
[cube sandbox](https://github.com/TencentCloud/CubeSandbox) - a KVM-based lightweight E2B-compatible sandbox. Quick iteration for parallel RL sessions and production services.

# Design detail

## Design decisions

- Guest builds live under `nixosConfigurations.<hostname>`.
- Runtime settings are resolved from the selected host entry in
  `nixosConfigurations`.
- The execution path is fixed to Linux `qemu:///session`, KVM, libvirt, virtiofs,
  NixOS, and home-manager.
- The host is not assumed to be NixOS. Each instance has its own dedicated
  `sysroot`. The `/nix` shown to the guest uses the instance `sysroot/nix`, not
  the host `/nix`.
- Host-side state lives only under `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
  `XDG_STATE_HOME`, and `XDG_RUNTIME_DIR`. `XDG_CACHE_HOME` is not used.
- Instance identity is `machine-id` (32 hex chars): `machine-prefix` (24 hex,
  persisted in `<active-config>/machine-prefix` on first resolve) + `hostname-hash`
  (sha256 of hostname, first 8 hex). `machine-id` is the guest machine-id
  source and the libvirt UUID source.
- `instance-id` is the dir/domain-name form `<dirname>-<hostname>-<machine-id>`
  (display prefix + match key). Lookup matches by `*-<machine-id>` so workspace
  rename/move stays transparent; the libvirt domain name reuses `instance-id`.
- Local scope (`<workspace>/.agentsandbox/machine-prefix`): worktree sharing vs.
  isolation is controlled by git tracking of the prefix (tracked = shared,
  untracked = each worktree regenerates on first use).
- Global scope (`$XDG_CONFIG_HOME/agentsandbox/machine-prefix`): single instance
  per hostname shared across all workspaces using the global config.
- No extra `current-system` link or host-state metadata JSON is kept.
- Place `sysroot/` next to `persistent/`.
- `allowed_hosts` and `mounts` are plain-text files and are always copied from
  the template. An empty `allowed_hosts` file remains deny-by-default.
- Generic HTTP filter DSL, `filter-default`, `block-domain`, HTTP ask mode,
  `proxy filter generate`, and CIDR cache are not part of the design.
- `.git/config` is not inherited, so `.git/config` sanitization is also not part
  of the design.
- `mutableSandboxConfig` is a bool and is used to protect `.agentsandbox` inside
  the workspace.
- The initial workspace mount appears in the guest as
  `/persistent/workspace/<dirname>`. It is stored in the same dynamic `mounts`
  file as directories added later with `mount`.
- Dynamic mounts are materialized under `/persistent/workspace` in a private
  host mount namespace and are exported through the single `/persistent`
  virtiofs share.
- OpenSnitch is optional, and its endpoint is provided by the selected
  `nixosConfiguration`. If the connection to the host is lost, treat it as a
  security breach and let the watchdog destroy the VM.
- Logs live under `logs/`. Active log files are uncompressed, and rotated
  archives use zstd compression. Runtime sockets and pid files live under
  `XDG_RUNTIME_DIR`.
- Generate the libvirt domain XML as a path in the Nix store and start the
  transient domain with `virsh create`.
- Apply the port-forward set when the domain is created. Reflect changes by
  recreating the transient domain.

## Configuration resolution

- The local config search target is the first `.agentsandbox/flake.nix` found
  while walking upward from the workspace.
- If no local config is found, use `$XDG_CONFIG_HOME/agentsandbox/flake.nix`.
- `agentsandbox init` creates `.agentsandbox/` in the current directory and
  copies `{flake.nix,configuration.nix,allowed_hosts,mounts}` into it.
- `agentsandbox init --global` copies
  `{flake.nix,configuration.nix,allowed_hosts,mounts}` to
  `$XDG_CONFIG_HOME/agentsandbox/`.
- The active config dir is treated as unique across the launcher and is used as
  the edit target for `allowed_hosts` and `mounts`, the flake build target, and
  the target for `mutableSandboxConfig` checks.

## Flake contract

- The active config dir is either local `.agentsandbox` or
  `$XDG_CONFIG_HOME/agentsandbox`.
- `nixosConfigurations.<hostname>` is the guest build contract.
- The launcher uses
  `nixosConfigurations.<hostname>.config.system.build.toplevel` as the build
  output and boot source.
- Runtime configuration values are read from the selected host under
  `nixosConfigurations` and consumed directly by the launcher.
- The launcher builds the dynamic mount set from the active config dir's
  `mounts` file. It includes the initial workspace mount and any additional
  mounts added through `mount`.
- The OpenSnitch endpoint lives at
  `services.opensnitch.settings.Server.Address` in the selected host config.

## Instance layout

- Split host state per instance as follows.

```text
$XDG_CONFIG_HOME/agentsandbox/
  flake.nix
  configuration.nix
  allowed_hosts
  mounts

$XDG_DATA_HOME/agentsandbox/<instance-id>/
  sysroot/
  persistent/

$XDG_STATE_HOME/agentsandbox/<instance-id>/
  logs/
    runtime.log
    requests.jsonl
    runtime-*.log.zst
    requests-*.jsonl.zst

$XDG_RUNTIME_DIR/agentsandbox/<instance-id>/
  lock
  ... runtime pid/socket files for helpers and sidecars
```

- `sysroot/` contains an instance-specific Nix root and the source of guest boot
  artifacts.
- `persistent/` is exported to the guest as `/persistent`.
- `mounts` stores the dynamic mount set for the active config, including the
  initial workspace mount.
- `logs/` stores the active log files and their rotated archives.
- The runtime dir contains instance-scoped sockets, pid files, and helper state
  for mount namespace and sidecars.
- Runtime filename details are implementation details and are not a fixed
  public contract.

## Build flow

- The launcher resolves active config dir (`.agentsandbox` upward search, else
  `$XDG_CONFIG_HOME/agentsandbox`) and selected `hostname`.
- The launcher resolves `instance-id` and instance paths under XDG roots.
- The launcher creates instance directories:
  - data: `sysroot/`, `persistent/`
  - state: `logs/`
  - runtime: pid/socket/lock files
- If `sysroot/nix/var/nix/profiles/default` is missing, bootstrap `sysroot`
  from Docker image `nixos/nix` (linux/amd64 manifest).
- If `--bootstrap` is specified, or system profile is missing, write template
  config into `sysroot/etc/nixos` and build initial profile with:
  `nix build /etc/nixos#nixosConfigurations.<hostname>.config.system.build.toplevel`
  in a mapped user+mount namespace.
- During mapped namespace setup, child enters `NEWUSER|NEWNS`, parent writes
  uid/gid mappings (`newuidmap`/`newgidmap`), then child continues.
- VM startup path:
  1. start mount supervisor in mapped namespace
  2. apply dynamic mounts to exported `/persistent`
  3. start `virtiofsd` on runtime socket
  4. render domain XML by executing `<system-profile>/domain.xml.sh`
  5. write XML to `<runtime-dir>/domain.xml`
  6. `virsh create <runtime-dir>/domain.xml`
- After boot path is available, run guest-side rebuild over SSH:
  `nixos-rebuild boot|switch --flake /persistent/etc/nixos#<hostname>`.
- Guest-centered rebuild is the security boundary: flake evaluation/build for
  the operational system runs inside the guest path rather than host runtime.
- If `domain.xml.sh` output differs from the runtime `domain-profile`, domain
  changes are applied by destroy+recreate semantics.
- On `up` for a running VM, guest rebuild uses `nixos-rebuild switch`; on
  `build` (non-up), guest rebuild uses `nixos-rebuild boot`.
- After guest rebuild on a running VM, the launcher compares
  `domain-profile/domain.xml.sh` and `new-system-profile/domain.xml.sh`.
  - If unchanged, the launcher keeps the current transient domain and runs
    `systemctl isolate multi-user.target` inside the guest.
  - If changed, the launcher applies restart semantics by recreating the
    transient domain.
- Security design intent: this split minimizes unnecessary domain recreation
  (smaller control-plane disruption) while ensuring virtualization-boundary
  changes from `domain.xml.sh` are never partially applied.
- Threat model intent: guest-side flake execution is assumed potentially
  adversarial; host-side behavior therefore limits itself to deterministic,
  narrow actions (profile comparison, domain recreate-or-continue decision)
  instead of broad host execution of flake-defined logic.

## Runtime contracts (implementation-level)

- Domain profile contract:
  - Runtime symlink: `<runtime-dir>/domain-profile -> <system-profile>`
  - SSH/port resolution and other runtime reads must use this profile.
- Domain XML input contract (`domain.xml.sh` environment):
  - `NIX_DIR`: `<sysroot>/nix`
  - `UID_MAP`, `GID_MAP`: host-compatible idmap strings
  - `INSTANCE_ID`: resolved instance id
  - `DOMAIN_UUID`: UUID derived from `machine-id`
  - `MACHINE_ID`: 32-hex machine id
  - `AGENTSANDBOX_BUILD`: `"1"` on build path, empty otherwise
  - `PERSISTENT_SOCKET_XML`: escaped virtiofs socket path fragment
- SSH port resolution contract:
  - Read `<domain-profile>/port-forwards`
  - Parse rows: `<name>\t<proto>\t<host-start>\t<host-end>\t<guest>`
  - Select `proto=tcp` row covering guest port 22 and compute host port by
    range offset.
- Mounts file contract:
  - File: `<active-config>/mounts`
  - Row format: `<host-path>\t<guest-name>`
  - Relative host paths are resolved from `workspace`.
  - `mount`/`unmount` edits this file; runtime reload is signaled by `HUP` to
    supervisor pid.
- Mutable sandbox config contract:
  - `mutableSandboxConfig` is represented by marker file
    `<system-profile>/mutable-sandbox-config`.
  - If marker is absent, config mount is remounted read-only.
- Policy file protection:
  - `allowed_hosts` and `mounts` inside guest-visible config path are always
    bind-mounted and remounted read-only.

## Guest system contract

- The guest boots with direct kernel boot.
- The guest root filesystem uses tmpfs, and `/nix` and `/persistent` are
  mounted with virtiofs.
- `/nix` exports instance `sysroot/nix` as `read-write,nodev`.
- `/persistent` exports instance `persistent/` as `read-write,nodev`.
- The mount entry corresponding to the startup workspace root appears at
  `/persistent/workspace/<dirname>`.
- Additional mount entries managed by `mount` and `unmount` appear at
  `/persistent/workspace/<guest-name>`.
- The guest `machine-id` is set via `systemd.machine_id=` on the kernel command
  line, using the instance `machine-id` value (no host-side file).
- The guest home-manager profile keeps shell and tool integration inside the
  guest, as in `v1_bwrap`.
- The guest persistent home uses `/persistent/home/vscode`.
- `~/.local/bin`, `.npm-global`, `.local/share/*`, and `.local/state/*` are kept
  as the guest-home compatibility layer.

## Mount export

- The startup workspace root is the directory containing `.agentsandbox` when
  local config exists, and the startup `cwd` for project-less execution.
- The active config dir contains a plain-text `mounts` file next to
  `allowed_hosts`.
- Each line of `mounts` represents one entry as `<host-path><TAB><guest-name>`.
- The launcher ensures that `mounts` contains an entry for the startup
  workspace root, and the guest-visible name of that entry is the basename of
  the startup workspace root.
- `mount` adds entries to `mounts`, and `unmount` removes them.
- The launcher starts a helper in a private host mount namespace and builds a
  synthetic tree under the exported `persistent/workspace` root.
- For instances with `mutableSandboxConfig = false`, the startup workspace
  entry is materialized with `.agentsandbox` read-only inside that synthetic
  tree.
- For instances with `mutableSandboxConfig = true`, the startup workspace entry
  is materialized without that protection.
- Additional mount entries are materialized as bind mounts under
  `persistent/workspace/<guest-name>` in the same namespace.
- The single `virtiofsd` instance for `/persistent` exports that synthetic tree
  to the guest.
- `mount` and `unmount` update both the `mounts` file and the helper namespace
  state for the running instance.
- The active config dir for the build is always reachable from the launcher.

## Network and proxy

- VM networking uses libvirt user networking with `passt`.
- `portForwards` is represented as an array of `{ proto, host, guest }` for
  host-to-guest publication only.
- Apply all `portForwards` from the selected host config when the domain is
  created.
- The `ssh` subcommand uses the `tcp` forward where `guest = 22`.
- Gateway restriction is enforced with a guest-side firewall, and host-gateway
  paths are concentrated on the proxy port and the OpenSnitch forward port.
- `allowed_hosts` uses the plain-text file in the active config dir.
- `allow-domain` appends normalized host/glob entries after deduplication.
- `unallow-domain` removes exact-match lines.
- Allowlist decisions use normalized hosts as keys.
- Even if vendor-specific live expansion is added in the future, the allowlist
  file itself remains plain-text host/glob.

## Proxy pipeline

- The proxy runs on the host side and receives the VM's HTTP/S egress.
- The request pipeline flows in the following order.
  1. request capture
  2. credential injection
  3. host allowlist check
  4. redaction
  5. upstream dispatch
- The only injector explicitly supported in v1 is GitHub.
- A redaction rule has either `pattern` or
  `source = value|env|file|env_file_key`.
- Conflicts between injected credentials and redaction rules are checked at
  proxy startup.
- Request logs are recorded as JSONL and include response metadata, filter
  results, and redaction results.
- Optional remote receivers are `syslog`, `syslog-remote`, `otlp-http`, and
  `otlp-grpc`.

## OpenSnitch

- OpenSnitch exists as an optional feature.
- The selected `nixosConfiguration` contains
  `services.opensnitch.settings.Server.Address`.
- OpenSnitch transport is separate from `portForwards`.
- The launcher starts a helper that connects the host-side UI listener and the
  forward port visible from the guest.
- The guest daemon connects to the selected `Server.Address`.
- The watchdog monitors OpenSnitch connection state and destroys disconnected
  instances after the grace period.
- For instances that use OpenSnitch, the baseline is `DefaultAction = "deny"`
  and `InterceptUnknown = true`.

## Logging

- State logs are collected under `logs/`.
- The active request log file is `logs/requests.jsonl`.
- The active non-request log file is `logs/runtime.log`.
- Request logs append JSON lines to the active file.
- Non-request logs append line-oriented records with timestamp, component, and
  message.
- Rotation renames the previous active file to an archive with a timestamp
  suffix and compresses it with zstd.
- Non-request log archives use `logs/runtime-*.log.zst`.
- Request log archives use `logs/requests-*.jsonl.zst`.
- `proxy-logs` reads the request log series.
- Diagnostics from the launcher, proxy, watchdog, and virtiofsd helpers are
  collected in `logs/runtime.log`.

## Lifecycle

- `build`
  - Resolve config/instance, prepare instance dirs.
  - Bootstrap sysroot/default profile when absent.
  - Ensure `flake.lock` exists for active config.
  - If VM is down (`down`/`shut off`/`crashed`): start VM, run guest
    `nixos-rebuild boot`, then explicitly return to down by `virsh destroy`.
  - If VM is running: run guest `nixos-rebuild boot`.
  - With `--bootstrap`: force initial profile rebuild path first.
  - `build` is a reconciliation operation, not a persistent runtime transition;
    it may boot temporarily for guest-side realization but returns to a non-running end state when started from down.
- `up`
  - Same build path as `build`, but keep VM running.
  - If VM is running, use guest `nixos-rebuild switch`.
  - Attach to guest shell by default; skip attach with `--detach`.
- `down` requests guest shutdown.
- `kill`
  - `virsh destroy`.
- `pause` / `unpause`
  - Apply `virsh suspend` / `virsh resume` to all instance ids sharing current
    machine-prefix scope.
- `destroy` (alias: `destory`)
  - Always attempts `virsh destroy` first.
  - `--system`: remove `sysroot/` (mapped namespace path)
  - `--data`: remove `persistent/` (mapped namespace path)
  - `--system --data`: remove whole data dir
  - `--logs`: remove whole state dir
  - `--conf`: remove resolved config dir
- `stats`
  - Uses `virsh domstats --raw --state --cpu-total --vcpu --balloon`.
  - Prints: state code/reason, cpu time/user/system ns, vcpu current/maximum,
    balloon current/rss/available/usable KiB.
- `verify`
  - Host side: `nix-store --verify --check-contents --repair --store local?root=<sysroot>`
  - Guest side (running VM): `nixos-rebuild build --repair --flake /persistent/etc/nixos#<hostname>`
