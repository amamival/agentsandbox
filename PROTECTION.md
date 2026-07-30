# DevVM Protection Model PRD

## Status

Draft. This document records the protection model that should guide the next implementation pass.

## Problem

DevVM runs untrusted development workloads in a real NixOS VM. Those workloads may include
LLM-generated commands, npm packages, flake inputs, build scripts, service definitions, and kernel-facing
software. A malicious or compromised package may try to exploit the guest kernel, gain guest root, and
then change the sandbox so that a later host action trusts attacker-controlled state.

The current implementation mixes several mutation paths:

- `devvm build` and `devvm up` may generate or refresh `flake.lock`.
- The guest can run `nixos-rebuild`, `switch-to-configuration`, or a toplevel `activate` script.
- The active configuration is mounted at runtime.
- Libvirt domain XML is generated from the NixOS system profile.
- Runtime source mounts are configured through `devvm.toml` and optional
  `devvm.local.toml` overrides.

The product needs a clear model for which side may mutate each state source, which mutations are only
temporary inside the guest, and which mutations may be reflected back into host-controlled policy.

## Threat Model

The guest workload is untrusted. This includes agent commands, project source code, package manager
scripts, Nix derivations, flake inputs, services, and processes running as guest root.

Guest kernel compromise is in scope. A workload may exploit a guest kernel or privileged guest service.
After that point, the attacker may have effective guest root and may be able to manipulate guest mount
namespaces, filesystems, processes, and runtime state.

The host must remain safe. The attacker must not be able to read arbitrary host files, write arbitrary
host files, reuse host Nix state, alter host firewall or libvirt policy, or cause host-controlled policy
to be silently replaced by guest-generated policy.

The template flake shipped by DevVM is trusted. `nix-store --verify` against the instance store is
trusted. The host launcher, host kernel, KVM, QEMU, libvirt, and virtiofs are trusted for this design.

The guest Nix store is instance-local. `/nix` inside the guest resolves to the instance sysroot, not to the
host `/nix`. Build and lock operations for the active sandbox configuration must not depend on a host `nix`
binary or host Nix store.

This model does not claim to prevent every action by guest root inside the live guest. Guest root can create
local stores, execute direct activation fragments, remount filesystems after a kernel compromise, or patch
live runtime state. The product goal is to protect the host and to prevent standard or persistent guest-side
configuration changes from becoming host-trusted state without an explicit host action.

## Goals

- Keep host files and host Nix state out of the guest trust boundary.
- Make `build` and `up` use the instance sysroot Nix and the same store that the guest will use.
- Preserve a fast build path by avoiding repeated `path:` uploads when the configuration is in a git repo.
- Allow common development workflows where `nix build` and `nix develop` work in the guest.
- Provide stricter modes that can deny Nix store mutation or deny system profile mutation.
- Let users mount runtime source trees as read-write or read-only per mount entry.
- Allow `flake.lock` to be created or refreshed only when the user explicitly requests that behavior, except
  that a missing lock file is treated as an implicit request to write one.
- Prevent guest-autonomous changes to system profiles and activation state from being silently reflected
  into host-controlled policy.

## Non-goals

- Do not expose a single `protectionMode` enum in the implementation.
- Do not make git a hard requirement for local `.devvm` configurations.
- Do not implement repository snapshots as the primary build source mechanism.
- Do not add a separate `runtimeSource` setting; runtime source exposure belongs to dynamic mounts.
- Do not try to make guest root powerless inside the live guest.
- Do not block direct hand-crafted activation fragments that bypass the standard NixOS entrypoints.

## Protection Concepts

Documentation may introduce named protection concepts, but implementation should stay as independent config
axes. The concepts are only presets or explanations.

### System Lab

The guest is allowed to mutate its system profile and Nix store. This is for NixOS module development,
learning, and workflows where running `nixos-rebuild switch`, `boot`, or `test` inside the guest is useful.

### Agent Workspace

The guest can build and develop with Nix, but cannot update Nix profiles through the standard profile paths.
This is the default target for coding agents. `nix build` and `nix develop` remain usable, while standard
`nixos-rebuild switch`, `boot`, and `test` should fail before persistent profile or activation changes.

### Sealed Run

The guest sees `/nix` as read-only. This is for audit, reproduction, or high-risk execution where building
new store paths is not needed.

## Configuration Axes

### `devvm.mutableSandboxConfig`

Controls whether the active sandbox configuration mount is writable from the guest.

When false, the guest must not be able to edit `.devvm` or the global sandbox configuration through
the runtime mount. Host commands remain the only supported way to apply configuration changes.

When true, the guest may edit the active sandbox configuration. This is useful for trusted interactive
configuration work and weakens the host-owned policy boundary.

### `devvm.mutableSystemProfile`

Controls whether `/nix/var/nix/profiles` is writable in the guest.

When false, standard Nix profile mutations are denied. This blocks normal `nixos-rebuild switch`,
`nixos-rebuild boot`, `nixos-rebuild test`, and profile-changing `nix profile` operations at the profile
update layer. It does not deny `nix build` or `nix develop` by itself.

When true, the guest may update system and user profiles. This implies `devvm.mutableNixStore = true`
because profile updates need a writable store and writable Nix state.

### `devvm.mutableNixStore`

Controls whether `/nix` is writable in the guest.

When false, `/nix`, `/nix/store`, and `/nix/var` are mounted read-only. This denies `nix build`,
`nix develop`, profile updates, garbage collection state changes, and normal store registration.

When true, Nix build and development workflows may write the instance store. This does not imply that
profiles are writable; that remains controlled by `devvm.mutableSystemProfile`.

### Mount Entry Mode

Runtime mounts carry their own mutability in TOML:

```toml
[mounts]
project = { source = ".", readonly = false }
source = { source = "src", readonly = true }
```

Omitted `readonly` means `false`. Read-only and read-write are mount entry
attributes, not global source policy.

## Build Source Model

`devvm build` and `devvm up` need a source tree that includes every relative path referenced by
the active flake. The build source should be chosen automatically to keep the build path reliable.

For a local `.devvm/flake.nix` that is tracked by git, the build source is the containing git worktree
root. The guest runs the rebuild from inside that worktree so that git-aware Nix input handling avoids the
slow repeated `path:` store upload.

For a local `.devvm/flake.nix` that is not tracked by git, git is not required. The build source stays
workspace-based and should preserve current behavior closely enough that relative paths from the flake keep
working.

For global config, the build source is
`$XDG_CONFIG_HOME/devvm/<project-name>`.

The build source is mounted read-only for normal builds. This source mount is an internal build/up mount,
separate from user-visible runtime mounts.

## Lock File Policy

`build` and `up` must not run host `nix flake lock` for the active sandbox configuration.

The default behavior with an existing `flake.lock` is no lock write. If the lock is stale, `nixos-rebuild`
should fail and the user should rerun with an explicit lock-writing flag.

Add a CLI flag named `--write-lock`. This flag allows `nixos-rebuild` to write `flake.lock` using the sysroot
Nix in the same mount namespace and the same instance store as the build.

If `flake.lock` is missing, behave as if `--write-lock` was specified. Before the file bind is installed, the
host creates the placeholder file:

```json
{"root":"","version":7}
```

The build source remains read-only. Only `flake.lock` is over-mounted as a writable file. The writable file
bind source must live outside the read-only source view so that the guest can rewrite the file even though the
parent directory is read-only.

`nixos-rebuild` should receive the appropriate lock flags directly. A separate pre-build `nix flake lock`
phase is not needed.

## System Mutation Guards

### Profile Guard

When `devvm.mutableSystemProfile = false`, `/nix/var/nix/profiles` is read-only. The path matters:
`/nix/var/nix/profiles/system` is a symlink, so the protected mount point is `/nix/var/nix/profiles`.

This denies standard profile updates and makes ordinary `nixos-rebuild switch`, `boot`, and `test` fail.

### `switch-to-configuration` Guard

The standard `switch-to-configuration` entrypoint takes
`/run/nixos/switch-to-configuration.lock` before it performs activation or systemd work. When system profile
mutation is disabled, DevVM should make that path fail to open as a file. A directory or read-only
mount at that path is sufficient.

This blocks the standard script early, including attempts with `NIXOS_NO_CHECK=1`.

### Activation Guard

Directly running a toplevel `activate` script can mutate `/bin/sh`, users, groups, persistent directories,
special filesystems, `/etc`, persisted files, and `/usr/bin/env` before it updates `/run/current-system`.
Protecting only `/run/current-system` is too late.

The template should install an early activation guard such as:

```sh
[ "$(readlink -f "$systemConfig")" = "$(readlink -f /nix/var/nix/profiles/system)" ] ||
  : > /nix/var/nix/profiles/.devvm-activation-guard || # Or try: touch /nix/var/nix/profiles
  exit 1 # and print some useful message for user
```

Activation of the current system profile is allowed. Activation of any other system only succeeds when the
system profile directory is writable. That lets the same system activate again in protected mode, while
allowing a new system activation only in modes where profile mutation was intentionally enabled.

## Runtime Mount Policy

Runtime source exposure is controlled by dynamic mounts. A workspace can be mounted read-write for normal
coding while `.devvm` is over-mounted read-only:

`devvm init` creates the workspace mount. Additional mounts use
`devvm mount [--read-only] <path> [name]`. The launcher recursively
over-mounts `devvm.toml` and `devvm.local.toml` read-only in the
guest-visible workspace and config trees.

## Expected CLI Behavior

- `devvm build` uses the sysroot Nix and instance store.
- `devvm up` uses the sysroot Nix and instance store.
- `devvm build` and `devvm up` fail on stale locks unless `--write-lock` is passed.
- Missing `flake.lock` is treated as implicit `--write-lock`.
- `--write-lock` makes only `flake.lock` writable inside an otherwise read-only build source.
- The guest runtime mount list accepts per-entry `rw` and `ro`.

## Acceptance Criteria

- A host without `nix` can run the active configuration lock/build path after bootstrap prerequisites exist.
- A missing `flake.lock` is created by the sysroot build path, not by host `nix`.
- A stale lock fails without `--write-lock`.
- A stale lock updates with `--write-lock`.
- A git-tracked `.devvm/flake.nix` builds from the git worktree root and avoids repeated `path:` uploads.
- An untracked `.devvm/flake.nix` still works without requiring git.
- With `mutableNixStore = true` and `mutableSystemProfile = false`, `nix build` works and standard
  `nixos-rebuild switch`, `boot`, and `test` fail.
- With `mutableNixStore = false`, `nix build` and `nix develop` fail because `/nix` is read-only.
- With `mutableSystemProfile = true`, guest `nixos-rebuild switch`, `boot`, and `test` are allowed.
- Guest-generated profile changes are never host-applied as host-controlled policy.
