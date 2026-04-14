#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
DEFAULT_ARTIFACT_ROOT="${SCRIPT_DIR}/result-virt-session"
DEFAULT_FLAKE_REF="path:${SCRIPT_DIR}#virt-session-boot-artifacts"
KERNEL=""
INITRD=""
INIT=""
CMDLINE=""
CMDLINE_FILE=""
ARTIFACT_ROOT=""
MEMORY_MIB="2048"
VCPUS="2"
MACHINE="q35"
ACCEL="kvm"
STATE_ROOT="${HOME}/.local/sandbox/qemu-virtiofs"
NIX_DIR="/nix"
PERSISTENT_DIR="${STATE_ROOT}/persistent"
WORKSPACE_DIR="${STATE_ROOT}/workspace"
SSH_USER="vscode"
ENABLE_NIX="1"
ENABLE_PERSISTENT="1"
ENABLE_WORKSPACE="1"
VIRTIOFSD_SANDBOX="none"
VIRTIOFSD_LOG_LEVEL="debug"
EXTRA_CMDLINE=(
  "console=ttyS0,115200n8"
  "systemd.journald.forward_to_console=1"
  "loglevel=7"
  "ignore_loglevel"
)
EXTRA_QEMU_ARGS=()
VIRTIOFSD_ARGS=()
SHARE_PIDS=()
SHARE_LOGS=()

function die() {
  echo "qemu-virtiofs.sh: $*" >&2
  exit 1
}

function usage() {
  cat <<'EOF'
usage: ./qemu-virtiofs.sh [options] [-- qemu-extra-args...]

Direct-QEMU repro that adds virtiofs back on top of qemu-minimal.sh.
It intentionally avoids libvirt and passt so that virtiofs can be isolated.

By default it exports:
  /nix        as tag "nix"        readonly
  ~/.local/.../persistent as tag "persistent"
  ~/.local/.../workspace  as tag "workspace"

The guest is still expected to fail later if its userspace needs more than this.
The point is to answer a narrower question:
  does direct QEMU still produce serial output once virtiofs is present?

With no explicit boot artifact arguments, this script builds or reuses the
dedicated VM boot artifacts from the local flake. It does not fall back to the
host's /run/current-system initrd.

options:
  --kernel PATH              guest kernel path
  --initrd PATH              guest initrd path
  --cmdline TEXT             guest kernel cmdline
  --cmdline-file PATH        file containing guest kernel cmdline
  --artifact-root PATH       directory containing kernel, initrd, init, kernel-params
  --memory MiB               guest memory in MiB; default: 2048
  --vcpus N                  guest vcpu count; default: 2
  --machine NAME             machine type; default: q35
  --accel NAME               accelerator; default: kvm
  --state-root PATH          state root; default: ~/.local/sandbox/qemu-virtiofs
  --nix-dir PATH             host path exported as /nix; default: /nix
  --persistent-dir PATH      host path exported as /persistent
  --workspace-dir PATH       host path exported as /workspace
  --skip-nix                 do not export /nix
  --skip-persistent          do not export /persistent
  --skip-workspace           do not export /workspace
  --virtiofs-sandbox MODE    virtiofsd sandbox: namespace|chroot|none; default: none
  --virtiofs-log-level LVL   virtiofsd log level; default: debug
  --virtiofsd-arg ARG        extra arg passed to every virtiofsd; may be repeated
  --append TEXT              append one extra kernel arg; may be repeated
  -h, --help                 show this help

examples:
  ./qemu-virtiofs.sh --accel tcg
  ./qemu-virtiofs.sh --skip-persistent --skip-workspace
  ./qemu-virtiofs.sh --virtiofs-sandbox namespace \
    --virtiofsd-arg '--uid-map=:0:1000:1:' \
    --virtiofsd-arg '--gid-map=:0:100:1:'
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --kernel)
      KERNEL="$2"
      shift 2
      ;;
    --initrd)
      INITRD="$2"
      shift 2
      ;;
    --cmdline)
      CMDLINE="$2"
      shift 2
      ;;
    --cmdline-file)
      CMDLINE_FILE="$2"
      shift 2
      ;;
    --artifact-root)
      ARTIFACT_ROOT="$2"
      shift 2
      ;;
    --memory)
      MEMORY_MIB="$2"
      shift 2
      ;;
    --vcpus)
      VCPUS="$2"
      shift 2
      ;;
    --machine)
      MACHINE="$2"
      shift 2
      ;;
    --accel)
      ACCEL="$2"
      shift 2
      ;;
    --state-root)
      STATE_ROOT="$2"
      PERSISTENT_DIR="${STATE_ROOT}/persistent"
      WORKSPACE_DIR="${STATE_ROOT}/workspace"
      shift 2
      ;;
    --nix-dir)
      NIX_DIR="$2"
      shift 2
      ;;
    --persistent-dir)
      PERSISTENT_DIR="$2"
      shift 2
      ;;
    --workspace-dir)
      WORKSPACE_DIR="$2"
      shift 2
      ;;
    --skip-nix)
      ENABLE_NIX="0"
      shift
      ;;
    --skip-persistent)
      ENABLE_PERSISTENT="0"
      shift
      ;;
    --skip-workspace)
      ENABLE_WORKSPACE="0"
      shift
      ;;
    --virtiofs-sandbox)
      VIRTIOFSD_SANDBOX="$2"
      shift 2
      ;;
    --virtiofs-log-level)
      VIRTIOFSD_LOG_LEVEL="$2"
      shift 2
      ;;
    --virtiofsd-arg)
      VIRTIOFSD_ARGS+=("$2")
      shift 2
      ;;
    --append)
      EXTRA_CMDLINE+=("$2")
      shift 2
      ;;
    --)
      shift
      EXTRA_QEMU_ARGS+=("$@")
      break
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

function require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

function absolutize() {
  local path="$1"
  readlink -f "$path"
}

function try_artifact_root() {
  local root="$1"
  [[ -n "$root" && -d "$root" ]] || return 1
  [[ -n "$KERNEL" ]] || [[ -r "${root}/kernel" ]] && KERNEL="${root}/kernel"
  [[ -n "$INITRD" ]] || [[ -r "${root}/initrd" ]] && INITRD="${root}/initrd"
  [[ -n "$INIT" ]] || [[ -r "${root}/init" ]] && INIT="${root}/init"
  [[ -n "$CMDLINE_FILE" || -n "$CMDLINE" ]] || [[ -r "${root}/kernel-params" ]] && CMDLINE_FILE="${root}/kernel-params"
  [[ -n "$KERNEL" && -n "$INITRD" && -n "$INIT" && ( -n "$CMDLINE_FILE" || -n "$CMDLINE" ) ]]
}

function build_dedicated_artifacts() {
  require_command nix
  printf 'building dedicated VM boot artifacts from %s\n' "$DEFAULT_FLAKE_REF" >&2
  nix build "$DEFAULT_FLAKE_REF" -o "$DEFAULT_ARTIFACT_ROOT" >&2
}

function autodetect_artifacts() {
  local candidate=''
  [[ -n "$KERNEL" && -n "$INITRD" && -n "$INIT" && ( -n "$CMDLINE_FILE" || -n "$CMDLINE" ) ]] && return 0
  for candidate in \
    "$ARTIFACT_ROOT" \
    "${SCRIPT_DIR}/result-virt-session" \
    "${SCRIPT_DIR}/result-virt-session-boot"
  do
    if try_artifact_root "$candidate"; then
      printf 'using dedicated VM boot artifacts from %s\n' "$candidate" >&2
      return 0
    fi
  done
  build_dedicated_artifacts
  if try_artifact_root "$DEFAULT_ARTIFACT_ROOT"; then
    printf 'using dedicated VM boot artifacts from %s\n' "$DEFAULT_ARTIFACT_ROOT" >&2
    return 0
  fi
  die "no dedicated VM boot artifacts found after building ${DEFAULT_FLAKE_REF}"
}

function ensure_artifacts() {
  autodetect_artifacts
  [[ -n "$KERNEL" ]] || die "--kernel is required"
  [[ -n "$INITRD" ]] || die "--initrd is required"
  [[ -n "$INIT" ]] || die "guest init path is required"
  [[ -n "$CMDLINE" || -n "$CMDLINE_FILE" ]] || die "--cmdline or --cmdline-file is required"
  [[ -r "$KERNEL" ]] || die "kernel is not readable: $KERNEL"
  [[ -r "$INITRD" ]] || die "initrd is not readable: $INITRD"
  [[ -r "$INIT" ]] || die "init is not readable: $INIT"
  if [[ -n "$CMDLINE_FILE" ]]; then
    [[ -r "$CMDLINE_FILE" ]] || die "cmdline file is not readable: $CMDLINE_FILE"
    CMDLINE="$(<"$CMDLINE_FILE")"
  fi
  [[ -n "$CMDLINE" ]] || die "kernel cmdline is empty"
  KERNEL="$(absolutize "$KERNEL")"
  INITRD="$(absolutize "$INITRD")"
  INIT="$(absolutize "$INIT")"
}

function build_cmdline() {
  local arg=''
  if [[ " $CMDLINE " != *" init="* ]]; then
    CMDLINE+=" init=${INIT}"
  fi
  for arg in "${EXTRA_CMDLINE[@]}"; do
    CMDLINE+=" ${arg}"
  done
}

function cpu_model() {
  if [[ "$ACCEL" == "kvm" ]]; then
    printf '%s\n' host
  else
    printf '%s\n' max
  fi
}

function cleanup() {
  local pid=''
  for pid in "${SHARE_PIDS[@]}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
}

function wait_for_socket() {
  local socket_path="$1" pid="$2" log_path="$3" attempts='0'
  while (( attempts < 50 )); do
    [[ -S "$socket_path" ]] && return 0
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      if [[ -r "$log_path" ]]; then
        tail -n 50 "$log_path" >&2 || true
      fi
      die "virtiofsd exited before socket became ready: ${socket_path}"
    fi
    attempts=$((attempts + 1))
    sleep 0.1
  done
  die "timed out waiting for virtiofsd socket: ${socket_path}"
}

function start_share() {
  local tag="$1" source_dir="$2" readonly="$3" index="$4"
  local socket_path="${STATE_ROOT}/fs${index}-${tag}.sock"
  local log_path="${STATE_ROOT}/fs${index}-${tag}.log"
  local -a cmd=()
  rm -f "$socket_path" "$log_path"
  cmd=(
    virtiofsd
    --socket-path "$socket_path"
    --shared-dir "$source_dir"
    --sandbox "$VIRTIOFSD_SANDBOX"
    --log-level "$VIRTIOFSD_LOG_LEVEL"
  )
  if [[ "$readonly" == "1" ]]; then
    cmd+=(--readonly)
  fi
  if [[ "${#VIRTIOFSD_ARGS[@]}" -gt 0 ]]; then
    cmd+=("${VIRTIOFSD_ARGS[@]}")
  fi
  "${cmd[@]}" >"$log_path" 2>&1 &
  SHARE_PIDS+=("$!")
  SHARE_LOGS+=("$log_path")
  wait_for_socket "$socket_path" "$!" "$log_path"
  printf '%s\n' "$socket_path"
}

function ensure_paths() {
  [[ -d "$NIX_DIR" || "$ENABLE_NIX" == "0" ]] || die "nix directory does not exist: $NIX_DIR"
  mkdir -p "$STATE_ROOT"
  [[ "$ENABLE_PERSISTENT" == "0" ]] || mkdir -p "$PERSISTENT_DIR/home/$SSH_USER"
  [[ "$ENABLE_WORKSPACE" == "0" ]] || mkdir -p "$WORKSPACE_DIR"
}

function preflight() {
  require_command qemu-system-x86_64
  require_command virtiofsd
  [[ -e /dev/fuse ]] || die "/dev/fuse is not present on the host"
  [[ -r /dev/fuse && -w /dev/fuse ]] || die "/dev/fuse is not accessible by current user"
  if [[ "$ACCEL" == "kvm" ]]; then
    [[ -e /dev/kvm ]] || die "/dev/kvm is not present on the host"
    [[ -r /dev/kvm && -w /dev/kvm ]] || die "/dev/kvm is not accessible by current user"
  fi
}

function main() {
  local cpu='' share_index='0'
  local socket_nix='' socket_persistent='' socket_workspace=''
  local -a qemu_args=()

  trap cleanup EXIT INT TERM

  ensure_artifacts
  build_cmdline
  ensure_paths
  preflight
  cpu="$(cpu_model)"

  if [[ "$ENABLE_NIX" == "1" ]]; then
    socket_nix="$(start_share nix "$NIX_DIR" 1 "$share_index")"
    share_index=$((share_index + 1))
  fi
  if [[ "$ENABLE_PERSISTENT" == "1" ]]; then
    socket_persistent="$(start_share persistent "$PERSISTENT_DIR" 0 "$share_index")"
    share_index=$((share_index + 1))
  fi
  if [[ "$ENABLE_WORKSPACE" == "1" ]]; then
    socket_workspace="$(start_share workspace "$WORKSPACE_DIR" 0 "$share_index")"
  fi

  printf 'kernel: %s\n' "$KERNEL" >&2
  printf 'initrd: %s\n' "$INITRD" >&2
  printf 'init: %s\n' "$INIT" >&2
  printf 'cmdline: %s\n' "$CMDLINE" >&2
  printf 'accel: %s\n' "$ACCEL" >&2
  printf 'virtiofs sandbox: %s\n' "$VIRTIOFSD_SANDBOX" >&2
  if [[ "${#SHARE_LOGS[@]}" -gt 0 ]]; then
    printf 'virtiofs logs:\n' >&2
    printf '  %s\n' "${SHARE_LOGS[@]}" >&2
  fi

  qemu_args=(
    -accel "$ACCEL"
    -machine "$MACHINE"
    -cpu "$cpu"
    -m "${MEMORY_MIB}M"
    -object "memory-backend-memfd,id=mem,size=${MEMORY_MIB}M,share=on"
    -numa "node,memdev=mem"
    -smp "$VCPUS"
    -no-reboot
    -display none
    -monitor none
    -nodefaults
    -chardev "stdio,id=char0,signal=off"
    -device "isa-serial,chardev=char0"
    -device virtio-rng-pci
    -kernel "$KERNEL"
    -initrd "$INITRD"
    -append "$CMDLINE"
  )

  if [[ -n "$socket_nix" ]]; then
    qemu_args+=(
      -chardev "socket,id=fs0,path=${socket_nix}"
      -device "vhost-user-fs-pci,queue-size=1024,chardev=fs0,tag=nix"
    )
  fi
  if [[ -n "$socket_persistent" ]]; then
    qemu_args+=(
      -chardev "socket,id=fs1,path=${socket_persistent}"
      -device "vhost-user-fs-pci,queue-size=1024,chardev=fs1,tag=persistent"
    )
  fi
  if [[ -n "$socket_workspace" ]]; then
    qemu_args+=(
      -chardev "socket,id=fs2,path=${socket_workspace}"
      -device "vhost-user-fs-pci,queue-size=1024,chardev=fs2,tag=workspace"
    )
  fi
  if [[ "${#EXTRA_QEMU_ARGS[@]}" -gt 0 ]]; then
    qemu_args+=("${EXTRA_QEMU_ARGS[@]}")
  fi

  qemu-system-x86_64 "${qemu_args[@]}"
}

main
