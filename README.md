# Agenthouse Sandbox

This repo boots a NixOS-based development sandbox on a local Linux host.

It gives you a real booted NixOS userland with `systemd`, persistence, and SSH access, while keeping guest root id-mapped instead of mapping directly to the host login user. The target is something closer to a lightweight rootless container VM than a `nix develop` shell.

This is a good fit for packaging, service work, NixOS modules, and NixOS learning in general. You can iterate on a real [`configuration.nix`](configuration.nix), rebuild, and observe how services, users, SSH, packages, and persistent state behave together without committing to a full VM workflow.

This is not a polished runtime. It is still an experimental launcher with a small amount of host-side setup to boot guest `systemd`.

The intended host platform is recent `amd64` Linux in general, not just NixOS. If this does not run on a reasonably current non-macOS Linux machine, that should be treated as a bug rather than an unsupported edge case.

The entrypoint is [`sandbox.sh`](sandbox.sh), which handles sysroot bootstrap, system build, container startup, re-attach, and workspace mounts.

## Requirements

Host-side dependencies:

- `bash`
- `curl`
- `tar`
- `jq`
- `sudo`
- `bubblewrap`
- `passt`
  requires the `pasta` binary
- `util-linux`
  requires `unshare`, `nsenter`, `mount`, `findmnt`, and `mountpoint`
- `procps`
  requires `pgrep` and `pkill`
- `gawk`

On NixOS, this is roughly enough:

```bash
nix-shell -p curl jq bubblewrap passt procps util-linux gawk
```

This is expected to work on recent `amd64` Linux generally, not just on NixOS hosts.
It also assumes working user namespaces and subordinate ID ranges for the current user.

You can sanity-check that with:

```bash
unshare --user --map-auto --setuid=0 --setgid=0 true && grep "^$USER:" /etc/subuid /etc/subgid
```

## What It Does

- pulls a `nixos/nix:latest` base sysroot from Docker Hub
- builds a NixOS system from [`flake.nix`](flake.nix) and [`configuration.nix`](configuration.nix)
- starts that system under `unshare`, `pasta`, and `bubblewrap` isolation
- attaches to the running sandbox with `nsenter`
- manages registered workspace mounts under `/sandbox/mounts`

Inside the sandbox, `systemd` runs as PID 1. SSH is enabled in the guest and forwarded to host port `2222`.

## Quick Start

Launch a sandbox container:

```bash
./sandbox.sh
```

The first boot may ask for `sudo` in order to create and prepare `/sandbox`.

Attach to the running sandbox:

```bash
./sandbox.sh
```

The script does both. If the sandbox is down, it bootstraps and starts it. If it is already up, it attaches to the guest namespaces.

When starting a stopped sandbox, it also rebuilds the guest system first. In practice, every fresh container start runs the equivalent of a `nixos-rebuild` before boot.

On a successful boot, the first terminal stays attached to the guest boot log and will typically stop at:

```text
[  OK  ] Reached target Multi-User System.
```

That is the expected steady state. Leave that tab alone, then either:

- open another terminal and run `./sandbox.sh` again to attach locally as root
- connect from your editor over SSH to `127.0.0.1:2222`

## Common Commands

```bash
./sandbox.sh build
./sandbox.sh up
./sandbox.sh down
./sandbox.sh exec -- bash
./sandbox.sh logs
./sandbox.sh logs -u sshd -n 50
./sandbox.sh ssh
```

- `./sandbox.sh`
  if the sandbox is running, attach; otherwise rebuild and start it
- `./sandbox.sh build`
  prepare `/sandbox` if needed and rebuild the guest system without starting it
- `./sandbox.sh up`
  rebuild and start the sandbox; fails if it is already running
- `./sandbox.sh down`
  ask guest `systemd` to shut down cleanly with `SIGRTMIN+3`
- `./sandbox.sh kill`
  send `TERM` to all processes in the sandbox PID namespace
- `./sandbox.sh pause`
  send `STOP` to all processes in the sandbox PID namespace
- `./sandbox.sh unpause`
  send `CONT` to all processes in the sandbox PID namespace
- `./sandbox.sh exec -- cmd ...`
  run a command in the running sandbox, or omit `cmd` to attach
- `./sandbox.sh logs [journalctl args ...]`
  run `journalctl` in the sandbox; default args are `-en1000`
- `./sandbox.sh ssh [ssh args ...]`
  run `ssh -p 2222 vscode@127.0.0.1 ...`

## Access

### Local Attach

Running `./sandbox.sh` again will find the guest `systemd` process in the same user namespace and enter it with `nsenter`.

To run a single command instead of attaching, use:

```bash
./sandbox.sh exec -- sh -lc 'id && pwd'
```

### SSH

OpenSSH is enabled in the guest. The relevant config lives in [`configuration.nix`](configuration.nix).

- host port: `2222`
- user: `vscode`
- password: empty

Example:

```bash
ssh vscode@127.0.0.1 -p 2222
```

This is also the intended path for VS Code Remote-SSH. Because the guest is forwarded to port `2222` rather than the default SSH port, either put the port in your SSH config or include `-p 2222` in the connection input. The UI does not always make it obvious that this field accepts free-form SSH text.

The guest config uses this concrete Remote-SSH style target:

```text
vscode@localhost -p 2222
```

## Caveats

- this currently supports only one sandbox instance at a time
- this currently expects an interactive tty
- startup performs a small amount of host-side setup outside `/sandbox`
- there is currently no cleanup for that host-side setup afterward
- builds are intentionally not reproducible, mainly because there is no `flake.lock`; bootstrap also starts from `nixos/nix:latest`

## Why This Exists

- VSCode DevContainers often fail to connect after rebuild, and Docker rebuild iteration are often too slow.
- This operates at a different layer than `nix develop`. A dev shell is useful when all you need is a process environment; this is for cases where you want a booted system with `systemd`, OpenSSH, persisted state, and room to exercise NixOS modules and services end to end.
- Codex-style sandboxes do not give you a full NixOS system configuration with booted services and persistent state. This does, without requiring a full VM workflow.
- Persistence is part of the point. You can throw away and rebuild the system layer without losing selected state, but you need to decide explicitly what should survive, such as toolchain caches, editor state, SSH host keys, package build artifacts, and long-lived service data.
- This works well for autonomous packaging, service work, and NixOS module authoring. It also pairs naturally with autonomous agents such as OpenClaw, because the agent gets a persistent, booted Linux target instead of a disposable shell.
- Compared to QEMU/KVM-style workflows, local file sharing is much simpler because you are not crossing a VM boundary or setting up another transport layer just to work on the same tree.
- It is intentionally stricter than rootless Podman or bwrap sandboxes that directly share the current host UID. Guest root is mapped to a separate subordinate ID range, so a breakout does not immediately inherit access to host credentials such as `~/.ssh` or API tokens.
- This is not a VM boundary. The trust and performance model sits in between a full VM and a current-UID-sharing sandbox.

## Networking

Guest networking is provided by `pasta`. The sandbox gets outbound network access, and guest SSH is forwarded to host port `2222`.

## Workspace Mounts

Registered workspace mounts are stored in `/sandbox/mounts`.

```bash
./sandbox.sh add ~/my-project
./sandbox.sh delete ~/my-project
./sandbox.sh mount
./sandbox.sh unmount
```

- `add`
  append paths to `/sandbox/mounts` and mount them immediately
- `delete`
  remove paths from `/sandbox/mounts` and unmount them
- `mount`
  mount every path listed in `/sandbox/mounts`
- `unmount`
  unmount every registered path in reverse order

Each host path `/host/path` is exposed in the guest as `/workspace/$(basename /host/path)`. The mount uses the running sandbox's current idmap so files owned by the host user appear as guest UID/GID `1000`.
That means `add` and `mount` expect a running sandbox.

## Host State

Runtime state lives under `/sandbox`:

- `/sandbox/sysroot`
- `/sandbox/persistent`
- `/sandbox/sysroot.pid`
- `/sandbox/mounts`

The first run needs `sudo` only to create and prepare `/sandbox`.

The guest is intentionally split between throwaway system state in `/sandbox/sysroot` and durable state mounted from `/sandbox/persistent`. In practice, that means the base system can be rebuilt or discarded while selected state survives across boots, but only if you explicitly list it in the guest configuration. The default setup keeps things such as `/etc/nixos`, logs, SSH host keys, workspace data, shell history, and tool caches.

Resetting the sandbox means deleting the sysroot and rebuilding it:

```bash
rm -rfv /sandbox/sysroot
```

## Stopping

Use the built-in subcommands:

```bash
./sandbox.sh down
./sandbox.sh kill
./sandbox.sh pause
./sandbox.sh unpause
```

- `down`
  clean guest shutdown through `systemd`
- `kill`
  send `TERM` to the whole sandbox PID namespace
- `pause` / `unpause`
  send `STOP` / `CONT` to the whole sandbox PID namespace

## Related Projects

- `devsandbox`: [devsandbox/README.md](devsandbox/README.md)
