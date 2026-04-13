#!/usr/bin/env bash
set -euo pipefail

SELF="$(basename "$0")"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
STATE_ROOT="${HOME}/.local/state/agenthouse-gvisor-exp"
ROOTFS="${STATE_ROOT}/rootfs"
PERSISTENT="${STATE_ROOT}/persistent"
WORKSPACE="${PERSISTENT}/workspace"
RUNSC_ROOT="${STATE_ROOT}/runsc"
LOG_DIR="${STATE_ROOT}/log"
SMOKE_BUNDLE="${STATE_ROOT}/bundle-smoke"
BUILD_BUNDLE="${STATE_ROOT}/bundle-build"
RUNTIME_BUNDLE="${STATE_ROOT}/bundle-runtime"
SMOKE_ID="agenthouse-gvisor-smoke"
BUILD_ID="agenthouse-gvisor-build"
RUNTIME_ID="agenthouse-gvisor-runtime"
ACTION="all"
ARCH="amd64"
TIMEOUT_SECS="180"
NETWORK_URL="https://example.com/"
SAMPLE_NAME="probe"
SAMPLE_SRC=""

function die() {
  echo "${SELF}: $*" >&2
  exit 1
}

function note() {
  echo "==> $*"
}

function usage() {
  cat <<'EOF'
usage: ./gvisor/experiment.sh [all|preflight|bootstrap|smoke|build|systemd|mount|cleanup|status]
                              [--state-root PATH] [--timeout SECS]
                              [--sample-src PATH] [--sample-name NAME]
                              [--network-url URL]

This script is an end-to-end feasibility probe for the gVisor design.
It checks the parts we actually need:
  - runsc can boot a container from a Nix rootfs
  - the repo's container-style NixOS build works under runsc
  - systemd can run as PID 1
  - host bind mounts staged under /persistent/workspace are visible in /workspace

actions:
  all        run preflight, bootstrap, smoke, build, systemd, and mount probe
  preflight  check host commands and repo inputs
  bootstrap  fetch nixos/nix rootfs into ~/.local/state/agenthouse-gvisor-exp/rootfs
  smoke      run nix --version inside gVisor on the fetched rootfs
  build      build nixosConfigurations.agenthouse inside gVisor
  systemd    boot /nix/var/nix/profiles/system/init and wait for systemd readiness
  mount      bind a host directory into persistent/workspace and verify /workspace visibility
  cleanup    unmount the probe path and force-delete runsc containers
  status     print runsc state for the runtime container

options:
  --state-root PATH  override state root
  --timeout SECS     readiness timeout; default: 180
  --sample-src PATH  host directory used for the mount probe
  --sample-name STR  guest mount name under /workspace; default: probe
  --network-url URL  URL fetched by the runtime network check
  -h, --help         show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    all|preflight|bootstrap|smoke|build|systemd|mount|cleanup|status)
      ACTION="$1"
      shift
      ;;
    --state-root)
      STATE_ROOT="$2"
      shift 2
      ;;
    --timeout)
      TIMEOUT_SECS="$2"
      shift 2
      ;;
    --sample-src)
      SAMPLE_SRC="$2"
      shift 2
      ;;
    --sample-name)
      SAMPLE_NAME="$2"
      shift 2
      ;;
    --network-url)
      NETWORK_URL="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

ROOTFS="${STATE_ROOT}/rootfs"
PERSISTENT="${STATE_ROOT}/persistent"
WORKSPACE="${PERSISTENT}/workspace"
RUNSC_ROOT="${STATE_ROOT}/runsc"
LOG_DIR="${STATE_ROOT}/log"
SMOKE_BUNDLE="${STATE_ROOT}/bundle-smoke"
BUILD_BUNDLE="${STATE_ROOT}/bundle-build"
RUNTIME_BUNDLE="${STATE_ROOT}/bundle-runtime"

function require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

function runsc_rootful() {
  sudo runsc --root "$RUNSC_ROOT" "$@"
}

function container_exists() {
  runsc_rootful state "$1" >/dev/null 2>&1
}

function container_status() {
  runsc_rootful state "$1" 2>/dev/null | jq -r '.status'
}

function container_running() {
  local status=''
  status="$(container_status "$1" || true)"
  [[ "$status" == "running" ]]
}

function force_delete_container() {
  local id="$1"
  if container_exists "$id"; then
    runsc_rootful kill "$id" KILL >/dev/null 2>&1 || true
    runsc_rootful delete --force "$id" >/dev/null 2>&1 || true
  fi
}

function ensure_state_dirs() {
  install -d -m 0755 "$STATE_ROOT" "$PERSISTENT" "$WORKSPACE" "$LOG_DIR"
  sudo install -d -m 0755 "$ROOTFS" "$RUNSC_ROOT"
}

function preflight() {
  local file=''
  note "checking host commands"
  for file in jq curl tar sudo runsc mount umount mountpoint realpath timeout awk; do
    require_command "$file"
  done
  [[ -r /etc/resolv.conf ]] || die "host /etc/resolv.conf is not readable"
  for file in flake.nix flake.lock configuration.nix bwrap-seccomp.c; do
    [[ -r "$SCRIPT_DIR/$file" ]] || die "missing gvisor input: $file"
  done
  [[ "$TIMEOUT_SECS" =~ ^[0-9]+$ ]] || die "--timeout must be an integer"
  [[ "$SAMPLE_NAME" =~ ^[A-Za-z0-9._-]+$ ]] || die "--sample-name must be basename-safe"
  runsc --version >/dev/null 2>&1 || die "runsc is installed but not runnable"
}

function fetch_rootfs() {
  local token='' index_json='' manifest_json='' digest='' blob='' rootfs_done=''
  local registry='https://registry-1.docker.io/v2'
  local repo='nixos/nix'
  local manifest_list_accept='application/vnd.docker.distribution.manifest.list.v2+json'
  local manifest_accept='application/vnd.docker.distribution.manifest.v2+json'

  [[ -d "$ROOTFS/nix/store" ]] && return 0

  note "fetching ${repo}:latest rootfs for linux/${ARCH}"
  token="$(
    curl -fsSL \
      "https://auth.docker.io/token?service=registry.docker.io&scope=repository:${repo}:pull" |
      jq -r '.token'
  )"
  [[ -n "$token" && "$token" != null ]] || die "failed to obtain Docker Hub token"

  index_json="$(
    curl -fsSL \
      -H "Authorization: Bearer ${token}" \
      -H "Accept: ${manifest_list_accept}" \
      "${registry}/${repo}/manifests/latest"
  )"
  digest="$(
    jq -r --arg arch "$ARCH" '
      .manifests[]
      | select(.platform.os == "linux" and .platform.architecture == $arch)
      | .digest
    ' <<<"$index_json" | head -n1
  )"
  [[ -n "$digest" ]] || die "no linux/${ARCH} manifest found for ${repo}:latest"

  manifest_json="$(
    curl -fsSL \
      -H "Authorization: Bearer ${token}" \
      -H "Accept: ${manifest_accept}" \
      "${registry}/${repo}/manifests/${digest}"
  )"
  rootfs_done=''
  while IFS= read -r blob; do
    [[ -n "$blob" ]] || continue
    curl -fsSL \
      -H "Authorization: Bearer ${token}" \
      "${registry}/${repo}/blobs/${blob}" |
      sudo tar -xf - -C "$ROOTFS" --same-owner --same-permissions
    rootfs_done='1'
  done < <(jq -r '.layers[].digest' <<<"$manifest_json")

  [[ -n "$rootfs_done" ]] || die "image manifest contained no layers"
  [[ -d "$ROOTFS/nix/store" ]] || die "rootfs extraction finished but nix/store is missing"
}

function copy_repo_inputs() {
  local src='' dst_dir="${ROOTFS}/etc/nixos"
  note "copying repo inputs into ${dst_dir}"
  sudo install -d -m 0755 "$dst_dir"
  for src in flake.nix flake.lock configuration.nix bwrap-seccomp.c; do
    sudo install -m 0644 "$SCRIPT_DIR/$src" "$dst_dir/$src"
  done
}

function write_smoke_bundle() {
  local tmp=''
  note "writing smoke bundle"
  rm -rf "$SMOKE_BUNDLE"
  install -d -m 0755 "$SMOKE_BUNDLE"
  ln -sfn "$ROOTFS" "$SMOKE_BUNDLE/rootfs"
  (
    cd "$SMOKE_BUNDLE"
    runsc spec -- /nix/var/nix/profiles/default/bin/nix --version
  )
  tmp="$(mktemp)"
  jq '
    .root.path = "rootfs" |
    .root.readonly = false |
    .process.terminal = false |
    .process.cwd = "/" |
    .mounts |= map(select(.destination != "/run" and .destination != "/tmp")) + [
      {
        "destination": "/run",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=755", "size=64m"]
      },
      {
        "destination": "/tmp",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=1777", "size=256m"]
      }
    ]
  ' "$SMOKE_BUNDLE/config.json" >"$tmp"
  mv "$tmp" "$SMOKE_BUNDLE/config.json"
}

function write_build_bundle() {
  local tmp=''
  note "writing build bundle"
  rm -rf "$BUILD_BUNDLE"
  install -d -m 0755 "$BUILD_BUNDLE"
  ln -sfn "$ROOTFS" "$BUILD_BUNDLE/rootfs"
  (
    cd "$BUILD_BUNDLE"
    runsc spec -- \
      /nix/var/nix/profiles/default/bin/nix \
      --extra-experimental-features "nix-command flakes" \
      build /etc/nixos#nixosConfigurations.agenthouse.config.system.build.toplevel \
      --out-link /nix/var/nix/profiles/system
  )
  tmp="$(mktemp)"
  jq '
    .root.path = "rootfs" |
    .root.readonly = false |
    .process.terminal = false |
    .process.cwd = "/" |
    .mounts |= map(
      select(
        .destination != "/etc/resolv.conf" and
        .destination != "/run" and
        .destination != "/run/lock" and
        .destination != "/tmp"
      )
    ) + [
      {
        "destination": "/etc/resolv.conf",
        "type": "bind",
        "source": "/etc/resolv.conf",
        "options": ["rbind", "ro"]
      },
      {
        "destination": "/run",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=755", "size=128m"]
      },
      {
        "destination": "/run/lock",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=1777", "size=4m"]
      },
      {
        "destination": "/tmp",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=1777", "size=512m"]
      }
    ]
  ' "$BUILD_BUNDLE/config.json" >"$tmp"
  mv "$tmp" "$BUILD_BUNDLE/config.json"
}

function write_runtime_bundle() {
  local tmp=''
  note "writing runtime bundle"
  rm -rf "$RUNTIME_BUNDLE"
  install -d -m 0755 "$RUNTIME_BUNDLE"
  ln -sfn "$ROOTFS" "$RUNTIME_BUNDLE/rootfs"
  (
    cd "$RUNTIME_BUNDLE"
    runsc spec -- /nix/var/nix/profiles/system/init
  )
  tmp="$(mktemp)"
  jq --arg persistent "$PERSISTENT" '
    .root.path = "rootfs" |
    .root.readonly = false |
    .hostname = "agenthouse-gvisor-exp" |
    .process.terminal = false |
    .process.cwd = "/" |
    .process.env |= ((. // []) + ["TERM=xterm-256color", "container=oci"]) |
    .mounts |= map(
      select(
        .destination != "/etc/resolv.conf" and
        .destination != "/persistent" and
        .destination != "/run" and
        .destination != "/run/lock" and
        .destination != "/tmp" and
        .destination != "/sys/fs/cgroup"
      )
    ) + [
      {
        "destination": "/etc/resolv.conf",
        "type": "bind",
        "source": "/etc/resolv.conf",
        "options": ["rbind", "ro"]
      },
      {
        "destination": "/persistent",
        "type": "bind",
        "source": $persistent,
        "options": ["rbind", "rw", "rprivate", "dcache=0"]
      },
      {
        "destination": "/run",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=755", "size=128m"]
      },
      {
        "destination": "/run/lock",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=1777", "size=4m"]
      },
      {
        "destination": "/tmp",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=1777", "size=512m"]
      },
      {
        "destination": "/sys/fs/cgroup",
        "type": "cgroup",
        "source": "cgroup",
        "options": ["nosuid", "noexec", "nodev", "relatime", "rw"]
      }
    ]
  ' "$RUNTIME_BUNDLE/config.json" >"$tmp"
  mv "$tmp" "$RUNTIME_BUNDLE/config.json"
}

function smoke_test() {
  preflight
  ensure_state_dirs
  fetch_rootfs
  write_smoke_bundle
  force_delete_container "$SMOKE_ID"
  note "running smoke test in gVisor"
  runsc_rootful --network=host --directfs=false --file-access=shared run \
    --bundle "$SMOKE_BUNDLE" "$SMOKE_ID"
}

function build_system() {
  preflight
  ensure_state_dirs
  fetch_rootfs
  copy_repo_inputs
  write_build_bundle
  force_delete_container "$BUILD_ID"
  note "building nixosConfigurations.agenthouse inside gVisor"
  runsc_rootful --network=host --directfs=false --file-access=shared run \
    --bundle "$BUILD_BUNDLE" "$BUILD_ID"
  [[ -x "$ROOTFS/nix/var/nix/profiles/system/init" ]] ||
    die "build finished but /nix/var/nix/profiles/system/init is missing"
}

function ensure_built_system() {
  [[ -x "$ROOTFS/nix/var/nix/profiles/system/init" ]] || build_system
}

function dump_runtime_logs() {
  if container_running "$RUNTIME_ID"; then
    runsc_rootful exec "$RUNTIME_ID" \
      /nix/var/nix/profiles/system/sw/bin/journalctl -b -n 200 --no-pager || true
  fi
}

function start_runtime() {
  note "starting runtime container"
  force_delete_container "$RUNTIME_ID"
  write_runtime_bundle
  runsc_rootful --network=host --directfs=false --file-access=shared create \
    --bundle "$RUNTIME_BUNDLE" "$RUNTIME_ID"
  runsc_rootful start "$RUNTIME_ID"
}

function wait_for_systemd() {
  local state=''
  note "waiting for systemd readiness"
  if ! timeout "${TIMEOUT_SECS}s" runsc_rootful exec "$RUNTIME_ID" \
    /nix/var/nix/profiles/system/sw/bin/systemctl is-system-running --wait; then
    dump_runtime_logs
    die "systemd did not become ready within ${TIMEOUT_SECS}s"
  fi
  state="$(
    runsc_rootful exec "$RUNTIME_ID" \
      /nix/var/nix/profiles/system/sw/bin/systemctl is-system-running
  )"
  [[ "$state" == "running" || "$state" == "degraded" ]] ||
    die "unexpected system state: ${state}"
  [[ "$(
    runsc_rootful exec "$RUNTIME_ID" \
      /nix/var/nix/profiles/system/sw/bin/bash -lc 'cat /proc/1/comm'
  )" == "systemd" ]] || die "PID 1 is not systemd"
}

function verify_runtime_network() {
  note "checking runtime network access"
  runsc_rootful exec "$RUNTIME_ID" \
    /nix/var/nix/profiles/system/sw/bin/bash -lc \
    "wget -qO- '$NETWORK_URL' >/dev/null"
}

function ensure_runtime() {
  preflight
  ensure_state_dirs
  ensure_built_system
  if ! container_running "$RUNTIME_ID"; then
    start_runtime
  fi
  wait_for_systemd
  verify_runtime_network
}

function ensure_sample_source() {
  local marker=''
  if [[ -n "$SAMPLE_SRC" ]]; then
    SAMPLE_SRC="$(realpath -e "$SAMPLE_SRC")"
    return 0
  fi
  SAMPLE_SRC="${STATE_ROOT}/sample-src"
  install -d -m 0755 "$SAMPLE_SRC"
  marker="host-marker-$(date +%s)"
  printf '%s\n' "$marker" >"$SAMPLE_SRC/marker.txt"
}

function mount_probe() {
  local guest_path="/workspace/${SAMPLE_NAME}"
  local live_value=''
  local target="${WORKSPACE}/${SAMPLE_NAME}"

  ensure_runtime
  ensure_sample_source
  install -d -m 0755 "$target"
  if mountpoint -q "$target"; then
    sudo umount "$target"
  fi

  note "binding ${SAMPLE_SRC} to ${target}"
  sudo mount --bind "$SAMPLE_SRC" "$target"
  runsc_rootful exec "$RUNTIME_ID" \
    /nix/var/nix/profiles/system/sw/bin/bash -lc \
    "test -f '${guest_path}/marker.txt'"

  live_value="live-$(date +%s)"
  printf '%s\n' "$live_value" >"$SAMPLE_SRC/live.txt"
  runsc_rootful exec "$RUNTIME_ID" \
    /nix/var/nix/profiles/system/sw/bin/bash -lc \
    "grep -Fxq '$live_value' '${guest_path}/live.txt'"

  note "unmounting ${target}"
  sudo umount "$target"
  runsc_rootful exec "$RUNTIME_ID" \
    /nix/var/nix/profiles/system/sw/bin/bash -lc \
    "test ! -e '${guest_path}/marker.txt'"
}

function cleanup() {
  local target="${WORKSPACE}/${SAMPLE_NAME}"
  note "cleaning up containers and probe mount"
  if mountpoint -q "$target"; then
    sudo umount "$target"
  fi
  force_delete_container "$SMOKE_ID"
  force_delete_container "$BUILD_ID"
  force_delete_container "$RUNTIME_ID"
}

function status() {
  if container_exists "$RUNTIME_ID"; then
    runsc_rootful state "$RUNTIME_ID"
  else
    echo "runtime container does not exist"
  fi
}

case "$ACTION" in
  preflight)
    preflight
    ;;
  bootstrap)
    preflight
    ensure_state_dirs
    fetch_rootfs
    copy_repo_inputs
    ;;
  smoke)
    smoke_test
    ;;
  build)
    build_system
    ;;
  systemd)
    ensure_runtime
    ;;
  mount)
    mount_probe
    ;;
  cleanup)
    preflight
    ensure_state_dirs
    cleanup
    ;;
  status)
    preflight
    ensure_state_dirs
    status
    ;;
  all)
    smoke_test
    build_system
    ensure_runtime
    mount_probe
    note "all probes passed"
    note "runtime is still running; use ./gvisor/experiment.sh cleanup to stop it"
    ;;
  *)
    die "unknown action: $ACTION"
    ;;
esac
