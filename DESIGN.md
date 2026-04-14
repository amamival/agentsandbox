# Agent Sandbox: an unshared, efficient, reproducible NixOS Linux VM for self-improving agentic workflows

In 2026, agentic loops are becoming increasingly unattended as LLM-based coding harnesses improve. However, the use of such harnesses and external LLM providers still raises security and privacy concerns.

Existing tools attempt to confine a session, but bubblewrap-based containers often map the current host user to root inside the container. If the workload escapes, it can quietly read the host user's `.ssh` credentials or API secrets as that user. In addition, when we want an agent to configure a Linux system directly, we may need to expose `CAP_SYS_ADMIN` (for rootless containers with `systemd`) and/or `/dev/kvm` (for QEMU/KVM), which exposes additional attack surface.

Running coding agents in *dangerous mode* - with full access to the local machine and the internet - is desirable for productivity. However, it can expose network fingerprints such as the hostname, usernames, MAC address, and network topology, and it can allow read/write access to files outside the session workspace through supply-chain attacks. A simpler approach, such as a NixOS container that reuses the host `/nix`, still makes it easy for a workload to identify vulnerable or profitable targets on the host.

## About

This repo boots a NixOS-based system sandbox on a local Linux host.

It gives you a real booted NixOS userland with `systemd`, persistence, and SSH access.

This is a good fit for packaging, service work, NixOS modules, and NixOS learning in general. You can iterate on a real [`configuration.nix`](configuration.nix), rebuild, and observe how services, users, SSH, packages, and persistent state behave together without sacrificing security and privacy.

This is not a polished runtime. It is still an experimental launcher that requires a small amount of host-side setup to boot guest `systemd`.

The target host platform is recent `amd64` Linux in general, not just NixOS. If this does not run on a reasonably current Linux machine, that should be treated as a bug rather than an unsupported edge case.

The entrypoint is [`agentsandbox`](agentsandbox), which handles sysroot bootstrap, system build, libvirt startup, attach, and workspace mounts.

## Installation

- NixOS - flake.nix
- Arch Linux - PKGBUILD
- Debian/Ubuntu - package release

## Using

- `agentsandbox`
  If the VM is running, attach to it; otherwise, rebuild and start it.

The subcommands are similar to those of **Docker Compose**.
```
help                Show help
version             Show version
doctor              Show diagnostics

init                Copy the initial `flake.nix` and `configuration.nix` into the current directory
build               Build the guest system
up                  Rebuild and start a VM; fails if it is already running
down                Tear down the VM gracefully
kill                Forcibly stop the VM
pause               Pause all running VMs
unpause             Unpause all running VMs
destroy             Delete the guest system while preserving persistent data

ps                  Show status of the VMs
ssh                 Connect to a regular user shell in a running VM. Equivalent to `ssh -p <port> vscode@127.0.0.1 ...`
exec                Execute a command in a running VM, or attach if omitted
logs                Show logs from a running VM. Runs `journalctl` with `-en1000` by default
stats               Display percentage of CPU, memory, network I/O, block I/O and PIDs for VMs
wait                Wait for running VMs to stop

mount               Mount a directory into a running VM now and on future starts, or show current mounts
unmount             Unmount a directory from a running VM now and on future starts

port                Prints the public port for a port binding.
allow-domain        Add a firewall rule that allows outbound traffic to a domain
unallow-domain      Remove the rule for the domain
proxy-logs          Follow MITM proxy logs
```
<!--[notimpl]
#systemd             create systemd unit file and register its compose stacks
#pull                pull stack images
#push                push stack images
#run                 create a VM similar to a service to run a one-off command
#start               start specific services
#stop                stop specific services
#restart             restart specific services
#config              displays the compose file
#images              List images used by the created VMs
-->

## Development

Nix users can run `nix develop --command ./agentsandbox`.\
Otherwise, install dependencies: **TODO**

## License

MIT

## Design notes

In our experiments, *gVisor* could not run *SystemD* as PID 1 because it lacks the `fsopen` and `rseq` syscalls.

## Similar project

[devsandbox](https://github.com/zekker6/devsandbox) - a `bwrap`-based container runtime with request logs, domain filtering, egress secret reduction, `GH_TOKEN` injection, desktop notification, and automatic sharing of tool configuration.
