# DevVM: a secure, efficient, reproducible NixOS Linux VM for self-improving agentic workflows

In 2026, agentic loops are becoming increasingly unattended as LLM-based coding harnesses improve. However, the use of such harnesses and external LLM providers still raises security and privacy concerns.

Existing tools attempt to confine a session, but bubblewrap-based containers often map the current host user to root inside the container. If the workload escapes, it can quietly read the host user's `.ssh` credentials or API secrets as that user. In addition, when we want an agent to configure a Linux system directly, we may need to expose `CAP_SYS_ADMIN` (for rootless containers with `systemd`) and/or `/dev/kvm` (for QEMU/KVM), which exposes additional attack surface.

Running coding agents in *dangerous mode* - with full access to the local machine and the internet - is desirable for productivity. However, it can expose network fingerprints such as the hostname, usernames, MAC address, and network topology, and it can allow read/write access to files outside the session workspace through supply-chain attacks. A simpler approach, such as a NixOS container that reuses the host `/nix`, still makes it easy for a workload to identify vulnerable or profitable targets on the host.

## About

This repo boots a NixOS-based system sandbox on a local Linux host.

It gives you a real booted NixOS userland with `systemd`, persistence, and SSH access.

This is a good fit for packaging, service work, NixOS modules, and NixOS learning in general. You can iterate on a real [`configuration.nix`](template/configuration.nix), rebuild, and observe how services, users, SSH, packages, and persistent state behave together without sacrificing security and privacy.

This is not yet a polished runtime. It remains an experimental launcher under heavy development.

The target host platform is recent `amd64` Linux in general, not just NixOS. If this does not run on a reasonably current Linux machine, that should be treated as a bug rather than an unsupported edge case.

The `devvm` command handles sysroot bootstrap, system builds, libvirt startup, attach, and mounts.

## Installation

- Nix Flakes: `nix run github:<OWNER>/<REPO>`
- Debian/Ubuntu, Fedora/RHEL, Arch: install packages from [GitHub Releases](https://github.com/<OWNER>/<REPO>/releases)
  - `.deb` / `.rpm` / `.pkg.tar.zst`

## Using

Linux host (x86_64/amd64) with KVM support is required.

- `devvm`
  If the VM is running, attach to it; otherwise, rebuild and start it.
- `devvm <command>`
  Run one of the commands below against the selected workspace/config/hostname.

The commands are similar to those of **Docker Compose**.
```
Usage: devvm [OPTIONS] [COMMAND]

Commands:
  version         Show version
  doctor          Show diagnostics
  init            Create `.devvm/` and write the initial template files
  build           Build the guest system
  up              Rebuild and start a VM; if already running, build and switch
  down            Tear down the VM gracefully
  kill            Forcibly stop the VM
  pause           Pause running VMs for all hostnames in the current config
  unpause         Unpause VMs for all hostnames in the current config
  destroy         Kill and delete guest files selected by flags (none by default)
  ls              List all VMs stored
  ps              List VM statuses for all hostnames in the current config
  ssh             Run a command as a user in a running VM, or attach if omitted
  exec            Run a command as root in a running VM, or attach if omitted
  logs            Show logs from a running VM. Runs `journalctl` with `-en1000` by default
  stats           Display statistics of CPU time, memory for VMs
  wait            Block until this VM becomes one of the states. Wait for stop states by default
  mount           Mount a file or directory into a running VM, or show mounts entries
  unmount         Unmount a file or directory from a running VM now and on future starts
  port            Prints the public port for a port binding
  allow-domain    Add a domain to the hostname-specific TOML policy
  unallow-domain  Remove a domain from the hostname-specific TOML policy
  proxy-logs      Follow MITM proxy logs
  verify          Verify and repair build
  audit           Run CVE scan against the guest store
  help            Print this message or the help of the given subcommand(s)

Options:
  -g, --global                       Use only global config (`$XDG_CONFIG_HOME/devvm`) and skip local upward search
  -p, --project-name <PROJECT_NAME>  Select project name. Combined with hostname to form the instance name
  -n, --hostname <HOSTNAME>          Select sandbox hostname (build target and instance identity input) [default: default]
  -w, --workspace <WORKSPACE>        Resolve the active workspace and config as if running from this directory
  -h, --help                         Print help
  -V, --version                      Print version
```

## Quick Start
```bash
# 1) Initialize local config in current workspace
devvm init
# 2) Build and start VM (attaches if startup succeeds)
devvm up
# 3) Open guest shell (user)
devvm ssh
# 4) Run command as root in guest
devvm exec -- uname -a
# 5) Stop VM gracefully
devvm down
```
For global (project-less) usage, initialize once with:
```bash
devvm --global init
```

## Development

Run development commands through `nix develop`, for example
`nix develop -c cargo run -- <subcommand> <options>`.
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
- `instance-id` and libvirt domain name are `<project-name>[<hostname>]`.
  `project-name` defaults to the workspace basename locally and `devvm`
  globally. The guest machine-id and libvirt UUID are derived from this ID.
- No extra `current-system` link or host-state metadata JSON is kept.
- Place `sysroot/` next to `persistent/`.
- Launcher policy is read from `devvm.toml` and optional
  `devvm.local.toml`. The local file and hostname sections override base
  policy.
- Generic HTTP filter DSL, `filter-default`, `block-domain`, HTTP ask mode,
  `proxy filter generate`, and CIDR cache are not part of the design.
- `.git/config` is not inherited, so `.git/config` sanitization is also not part
  of the design.
- The initial workspace mount appears in the guest as
  `/persistent/workspace/<dirname>`. It is stored in the TOML `mounts` table
  with directories added later through `mount`.
- Dynamic mounts are materialized under `/persistent/workspace` in a private
  host mount namespace and are exported through the single `/persistent`
  virtiofs share.
- Runtime sockets and pid files live under `XDG_RUNTIME_DIR`.
- Generate libvirt domain XML in Rust and start the transient domain with
  `virsh create`.
- Apply the port-forward set when the domain is created. Reflect changes by
  recreating the transient domain.

## Configuration resolution

- The local config search target is the first `.devvm/` directory or
  directory containing `devvm.toml` or `devvm.local.toml` found
  while walking upward from the workspace.
- If no local config is found, use
  `$XDG_CONFIG_HOME/devvm/<project-name>`.
- `devvm init` creates `.devvm/` in the current directory and
  writes the Nix files, `devvm.toml`, and runtime module files.
- `devvm init --global` writes the same files to
  `$XDG_CONFIG_HOME/devvm/<project-name>`.
- The active config dir is treated as unique across the launcher and is used as
  the TOML edit target and flake build target.

## Flake contract

- The active config dir is either local `.devvm` or
  `$XDG_CONFIG_HOME/devvm/<project-name>`.
- `nixosConfigurations.<hostname>` is the guest build contract.
- The launcher uses
  `nixosConfigurations.<hostname>.config.system.build.toplevel` as the build
  output and boot source.
- Runtime settings are merged from `devvm.toml`,
  `devvm.local.toml`, and their selected `[hosts.<hostname>]` sections.
- The launcher builds the dynamic mount set from the merged `mounts` table.

## Instance layout

- Split host state per instance as follows.

```text
$XDG_CONFIG_HOME/devvm/<project-name>/
  flake.nix
  configuration.nix
  devvm.toml
  devvm.local.toml  # optional

$XDG_DATA_HOME/devvm/<instance-id>/
  sysroot/
  persistent/

$XDG_STATE_HOME/devvm/<instance-id>/
  logs/
    runtime.log
    requests.jsonl
    runtime-*.log.zst
    requests-*.jsonl.zst

$XDG_RUNTIME_DIR/devvm/<instance-id>/
  lock
  ... runtime pid/socket files for helpers and sidecars
```

- `sysroot/` contains an instance-specific Nix root and the source of guest boot
  artifacts.
- `persistent/` is exported to the guest as `/persistent`.
- The merged TOML `mounts` table stores the dynamic mount set, including the
  initial workspace mount.
- `logs/` stores the active log files and their rotated archives.
- The runtime dir contains instance-scoped sockets, pid files, and helper state
  for mount namespace and sidecars.
- Runtime filename details are implementation details and are not a fixed
  public contract.

## Build flow

- The launcher resolves active config dir (`.devvm` upward search, else
  `$XDG_CONFIG_HOME/devvm/<project-name>`) and selected `hostname`.
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
  3. render domain XML in Rust from TOML and runtime paths
  4. write XML and port-forward state under `<runtime-dir>`
  5. `virsh create <runtime-dir>/domain.xml`
- After boot path is available, run guest-side rebuild over SSH:
  `nixos-rebuild boot|switch --flake /persistent/etc/nixos#<hostname>`.
- Guest-centered rebuild is the security boundary: flake evaluation/build for
  the operational system runs inside the guest path rather than host runtime.
- If newly rendered domain XML differs from the runtime XML, domain changes are
  applied by destroy+recreate semantics.
- On `up` for a running VM, guest rebuild uses `nixos-rebuild switch`; on
  `build` (non-up), guest rebuild uses `nixos-rebuild boot`.
- After guest rebuild on a running VM, the launcher renders and compares the
  old and new domain XML.
  - If unchanged, the launcher keeps the current transient domain and runs
    `systemctl isolate multi-user.target` inside the guest.
  - If changed, the launcher applies restart semantics by recreating the
    transient domain.
- Security design intent: this split minimizes unnecessary domain recreation
  (smaller control-plane disruption) while ensuring virtualization-boundary
  changes are never partially applied.
- Threat model intent: guest-side flake execution is assumed potentially
  adversarial; host-side behavior therefore limits itself to deterministic,
  narrow actions (profile comparison, domain recreate-or-continue decision)
  instead of broad host execution of flake-defined logic.

## Runtime contracts (implementation-level)

- SSH port resolution contract:
  - Read runtime JSON from `<runtime-dir>/port-forwards`.
  - Select `proto=tcp` row covering guest port 22 and compute host port by
    range offset.
- Mount contract:
  - Read the effective TOML `mounts` table.
  - Relative host paths are resolved from `workspace`.
  - `mount`/`unmount` edits `devvm.local.toml`; runtime reload is
    signaled by `HUP` to the supervisor pid.
- Policy file protection:
  - Every recursively discovered `devvm.toml` and
    `devvm.local.toml` in guest-visible workspace/config trees is
    mounted read-only. Hard-linked policy files are rejected.
- Audit command contract (`devvm audit`):
  - Resolve active flake dir and instance with the same path as other instance-scoped commands.
  - Execute host `vulnix` directly (no guest-side wrapper execution).
  - Prepend fixed arguments `-g <instance-sysroot>` so scan scope is the instance store root.
  - Forward all user audit arguments after the fixed prefix without launcher-side rewriting.
  - Inherit stdin/stdout/stderr to preserve vulnix I/O behavior and output format.
  - Terminate process with vulnix exit code; if no code is available (for example, signal),
    normalize to exit code `1`.
  - Runtime prerequisite is a patched host vulnix binary in launcher environment
    (operationally: run inside this repository's `nix develop`).

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
  line from a hash of the instance ID.
- The guest home-manager profile keeps shell and tool integration inside the
  guest, as in `v1_bwrap`.
- The guest persistent home uses `/persistent/home/vscode`.
- `~/.local/bin`, `.npm-global`, `.local/share/*`, and `.local/state/*` are kept
  as the guest-home compatibility layer.

## Host CA trust

- When `vm.useHostCerts = true`, the launcher imports one host-generated PEM
  bundle. It selects the first regular file from `SSL_CERT_FILE`,
  `/etc/ssl/certs/ca-certificates.crt`, `/etc/ssl/ca-bundle.pem`,
  `/etc/ssl/certs/ca-bundle.crt`, and
  `/etc/pki/tls/certs/ca-bundle.crt`, in that order.
- The launcher does not merge individual certificate files. On Ubuntu and
  Debian hosts, `update-ca-certificates` already combines distribution and
  locally managed roots, including corporate roots under
  `/usr/local/share/ca-certificates`, into
  `/etc/ssl/certs/ca-certificates.crt`.
- The selected bundle is exported read-only as `/persistent/host-ca.crt`, and
  the launcher adds `devvm.use-host-certs` to the guest kernel command
  line. If no bundle is found, VM startup fails before the domain is created.
- NixOS exposes `/etc/ssl/certs/ca-certificates.crt` as a symlink into the Nix
  store. A systemd mount unit cannot use that non-canonical path, so a
  conditional oneshot service resolves the symlink once, bind-mounts the host
  bundle on the canonical target, and remounts it read-only.
- The file mount pins the selected bundle for the lifetime of the VM. Host CA
  updates and corporate CA rotation take effect on the next VM start.
- This changes trust for software that uses the guest's default OpenSSL-style
  bundle. Applications with independent stores, such as Java keystores or
  browser-specific NSS databases, are outside this contract.

## Mount export

- The startup workspace root is the directory containing `.devvm` when
  local config exists, and the startup `cwd` for project-less execution.
- The effective TOML `mounts` table maps guest-relative names to host sources
  and a `readonly` flag.
- `init` adds the startup workspace mount under its basename.
- `mount` and `unmount` persist hostname-specific overrides in
  `devvm.local.toml`.
- The launcher starts a helper in a private host mount namespace and builds a
  synthetic tree under the exported `persistent/workspace` root.
- TOML policy files in the startup workspace are over-mounted read-only.
- Additional mount entries are materialized as bind mounts under
  `persistent/workspace/<guest-name>` in the same namespace.
- The single `virtiofsd` instance for `/persistent` exports that synthetic tree
  to the guest.
- `mount` and `unmount` update TOML and reload the helper namespace for a
  running instance.
- The active config dir for the build is always reachable from the launcher.

## Network and proxy

- VM networking uses libvirt user networking with `passt`.
- `portForwards` is a TOML table of named host-to-guest publications.
- Apply all `portForwards` from the selected host config when the domain is
  created.
- `port` prints the public host endpoint for a guest port binding.
- `port` accepts an optional `guest_port` argument to resolve guest-to-host
  port mapping.
- `port` accepts an optional `--protocol <tcp|udp>` filter.
- If `guest_port` is omitted, `port` returns all published bindings as
  `<name><TAB><proto><TAB><address>:<port><TAB><device>`.
- The `ssh` subcommand uses the `tcp` forward where `guest_port = 22`.
- `allowedHosts` is stored in TOML. `allow-domain` and `unallow-domain` edit
  hostname-specific local overrides.
- v0.2 does not enforce `allowedHosts` on network traffic.

## Future proxy pipeline (not implemented in v0.2)

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

## Future OpenSnitch integration (not implemented in v0.2)

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

## Future proxy logging (not implemented in v0.2)

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
  - Apply `virsh suspend` / `virsh resume` to all stored instances for the
    selected project.
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
