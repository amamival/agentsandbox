# Agenthouse Sandbox

This repo boots a NixOS-based development sandbox on a local Linux host.

It gives you a real booted NixOS userland with `systemd`, persistence, and SSH access, while keeping guest root id-mapped instead of mapping directly to the host login user. The target is something closer to a lightweight rootless container VM than a `nix develop` shell.

This is a good fit for packaging, service work, NixOS modules, and NixOS learning in general. You can iterate on a real [`configuration.nix`](/workspace/cont/configuration.nix), rebuild, and observe how services, users, SSH, packages, and persistent state behave together without committing to a full VM workflow.

This is not a polished runtime. It is still an experimental launcher with a small amount of host-side setup to boot guest `systemd`.

The intended host platform is recent `amd64` Linux in general, not just NixOS. If this does not run on a reasonably current non-macOS Linux machine, that should be treated as a bug rather than an unsupported edge case.

The entrypoint is [`start.sh`](/workspace/cont/start.sh), which handles sysroot bootstrap, system build, container startup, and re-attach.

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
  requires `unshare` and `nsenter`
- `procps`
  requires `pgrep`

On NixOS, this is roughly enough:

```bash
nix-shell -p curl jq bubblewrap passt procps
```

This is expected to work on recent `amd64` Linux generally, not just on NixOS hosts.
It also assumes working user namespaces and subordinate ID ranges for the current user.

You can sanity-check that with:

```bash
unshare --user --map-auto --setuid=0 --setgid=0 true && grep "^$USER:" /etc/subuid /etc/subgid
```

## What It Does

- pulls a `nixos/nix:latest` base sysroot from Docker Hub
- builds a NixOS system from [`flake.nix`](/workspace/cont/flake.nix) and [`configuration.nix`](/workspace/cont/configuration.nix)
- starts that system under `unshare`, `pasta`, and `bubblewrap` isolation
- attaches to the running sandbox with `nsenter`

Inside the sandbox, `systemd` runs as PID 1. SSH is enabled in the guest and forwarded to host port `2222`.

## Quick Start

Launch a sandbox container:

```bash
./start.sh
```

The first boot may ask for `sudo` in order to create and prepare `/sandbox`.

Attach to the running sandbox:

```bash
./start.sh
```

The script does both. If the sandbox is down, it bootstraps and starts it. If it is already up, it attaches to the guest namespaces.

When starting a stopped sandbox, it also rebuilds the guest system first. In practice, every fresh container start runs the equivalent of a `nixos-rebuild` before boot.

On a successful boot, the first terminal stays attached to the guest boot log and will typically stop at:

```text
[  OK  ] Reached target Multi-User System.
```

That is the expected steady state. Leave that tab alone, then either:

- open another terminal and run `./start.sh` again to attach locally as root
- connect from your editor over SSH to `127.0.0.1:2222`

## Access

### Local Attach

Running `./start.sh` again will find the guest `systemd` process in the same user namespace and enter it with `nsenter`.

### SSH

OpenSSH is enabled in the guest. The relevant config lives in [`configuration.nix`](/workspace/cont/configuration.nix).

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

## Host State

Runtime state lives under `/sandbox`:

- `/sandbox/sysroot`
- `/sandbox/persistent`
- `/sandbox/sysroot.pid`

The first run needs `sudo` only to create and prepare `/sandbox`.

The guest is intentionally split between throwaway system state in `/sandbox/sysroot` and durable state mounted from `/sandbox/persistent`. In practice, that means the base system can be rebuilt or discarded while selected state survives across boots, but only if you explicitly list it in the guest configuration. The default setup keeps things such as `/etc/nixos`, logs, SSH host keys, workspace data, shell history, and tool caches.

Resetting the sandbox means deleting the sysroot and rebuilding it:

```bash
rm -rfv /sandbox/sysroot
```

## Stopping

To stop the running sandbox, either interrupt the foreground boot log with `Ctrl-C` or run [`kill.sh`](/workspace/cont/kill.sh).

```bash
./kill.sh
```

## Related Projects

- `devsandbox`: [devsandbox/README.md](/workspace/cont/devsandbox/README.md)
