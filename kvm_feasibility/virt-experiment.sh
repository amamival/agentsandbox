#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
DEFAULT_ARTIFACT_ROOT="${SCRIPT_DIR}/result-virt-session"
DEFAULT_FLAKE_REF="path:${SCRIPT_DIR}#virt-session-boot-artifacts"
URI="qemu:///session"
NAME="agenthouse-virt-exp"
STATE_ROOT="${HOME}/.local/sandbox/virt-experiment"
NIX_DIR="/nix"
PERSISTENT_DIR=""
WORKSPACE_DIR=""
KERNEL=""
INITRD=""
INIT=""
CMDLINE=""
CMDLINE_FILE=""
ARTIFACT_ROOT=""
SSH_USER="vscode"
SSH_PORT="2223"
VCPUS="4"
MEMORY_MIB="8192"
SSH_WAIT_SECS="180"
KEEP_FAILED_VM="0"
ATTACH_CONSOLE="0"
ACTION="up"
VIRTIOFSD_BIN=""
VFIO_BDFS=()
VIRTIOFSD_SANDBOX="none"
VIRTIOFSD_LOG_LEVEL="debug"
FS_NIX_SOCKET=""
FS_PERSISTENT_SOCKET=""
FS_WORKSPACE_SOCKET=""

function die() {
  echo "virt-experiment.sh: $*" >&2
  exit 1
}

function usage() {
  cat <<'EOF'
usage: ./virt-experiment.sh [up|down|destroy|status|console|ssh|logs] [options] [-- command ...]

This is a simple end-to-end experiment for the VIRT.md session-mode design.
It expects a guest build that already:
  - boots with direct kernel boot
  - mounts /nix, /persistent, /workspace from virtiofs
  - starts systemd, sshd, and libvirtd
  - uses a vscode user whose UID/GID match the host user

With no explicit boot artifact arguments, this script builds or reuses the
dedicated VM boot artifacts from the local flake. It does not fall back to the
host's /run/current-system initrd.

options:
  --kernel PATH          guest kernel path
  --initrd PATH          guest initrd path
  --cmdline TEXT         guest kernel cmdline
  --cmdline-file PATH    file containing guest kernel cmdline
  --artifact-root PATH   directory containing kernel, initrd, init, kernel-params
  --nix-dir PATH         host path exported as /nix; default: /nix
  --persistent-dir PATH  host path exported as /persistent
  --workspace-dir PATH   host path exported as /workspace
  --state-root PATH      state root; default: ~/.local/sandbox/virt-experiment
  --name NAME            libvirt domain name; default: agenthouse-virt-exp
  --memory MiB           guest memory in MiB; default: 8192
  --vcpus N              guest vcpu count; default: 4
  --ssh-user USER        ssh user; default: vscode
  --ssh-port PORT        forwarded host port; default: 2223
  --ssh-wait SECS        readiness timeout; default: 180
  --vfio-bdf BDF         add a VFIO PCI device, e.g. 0000:03:00.0
  --attach-console       attach virsh console after the guest becomes ready
  --keep-failed-vm       keep the transient VM running on readiness failure
  -h, --help             show this help

examples:
  ./virt-experiment.sh up --kernel ./result/kernel --initrd ./result/initrd \
    --cmdline-file ./result/kernel-params
  ./virt-experiment.sh ssh
  ./virt-experiment.sh logs -- -u libvirtd -n 50
  ./virt-experiment.sh destroy
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    up|down|destroy|status|console|ssh|logs)
      ACTION="$1"
      shift
      ;;
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
    --state-root)
      STATE_ROOT="$2"
      shift 2
      ;;
    --name)
      NAME="$2"
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
    --ssh-user)
      SSH_USER="$2"
      shift 2
      ;;
    --ssh-port)
      SSH_PORT="$2"
      shift 2
      ;;
    --ssh-wait)
      SSH_WAIT_SECS="$2"
      shift 2
      ;;
    --vfio-bdf)
      VFIO_BDFS+=("$2")
      shift 2
      ;;
    --attach-console)
      ATTACH_CONSOLE="1"
      shift
      ;;
    --keep-failed-vm)
      KEEP_FAILED_VM="1"
      shift
      ;;
    --)
      shift
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

SSH_ARGS=("$@")
[[ -n "$PERSISTENT_DIR" ]] || PERSISTENT_DIR="${STATE_ROOT}/persistent"
[[ -n "$WORKSPACE_DIR" ]] || WORKSPACE_DIR="${STATE_ROOT}/workspace"
LOCKFILE="${STATE_ROOT}/lock"
DOMAIN_XML="${STATE_ROOT}/domain.xml"

function domstate() {
  virsh -c "$URI" domstate "$NAME" 2>/dev/null || true
}

function domain_running() {
  local state
  state="$(domstate)"
  [[ "$state" == "running" || "$state" == "paused" ]]
}

function require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

function check_port_free() {
  if ss -ltnH | awk -v port=":${SSH_PORT}" '$4 ~ port "$" { found = 1 } END { exit !found }'; then
    die "host port ${SSH_PORT} is already in use"
  fi
}

function check_nested_enabled() {
  local nested=''
  if [[ -r /sys/module/kvm_intel/parameters/nested ]]; then
    read -r nested < /sys/module/kvm_intel/parameters/nested
  elif [[ -r /sys/module/kvm_amd/parameters/nested ]]; then
    read -r nested < /sys/module/kvm_amd/parameters/nested
  fi
  [[ "$nested" == "Y" || "$nested" == "1" ]] || die "host nested KVM is not enabled"
}

function check_kvm_usable() {
  local pidfile='' pid='' capabilities=''
  pidfile="$(mktemp)"
  if ! qemu-system-x86_64 -accel kvm -machine none -nodefaults -nographic \
    -serial none -monitor none -display none -daemonize -pidfile "$pidfile" >/dev/null 2>&1; then
    rm -f "$pidfile"
    die "KVM acceleration is not usable; /dev/kvm exists but qemu-system-x86_64 -accel kvm failed"
  fi
  pid="$(<"$pidfile")"
  kill "$pid" >/dev/null 2>&1 || true
  rm -f "$pidfile"
  capabilities="$(virsh -c "$URI" capabilities)"
  [[ "$capabilities" == *"<domain type='kvm'/>"* ]] ||
    die "libvirt session does not advertise KVM; restart the user libvirt session after enabling KVM"
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
    "${STATE_ROOT}/result" \
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
  [[ -n "$KERNEL" ]] || die "--kernel is required for up"
  [[ -n "$INITRD" ]] || die "--initrd is required for up"
  [[ -n "$INIT" ]] || die "guest init path is required for up"
  [[ -n "$CMDLINE" || -n "$CMDLINE_FILE" ]] || die "--cmdline or --cmdline-file is required for up"
  [[ -r "$KERNEL" ]] || die "kernel is not readable: $KERNEL"
  [[ -r "$INITRD" ]] || die "initrd is not readable: $INITRD"
  [[ -r "$INIT" ]] || die "init is not readable: $INIT"
  if [[ -n "$CMDLINE_FILE" ]]; then
    [[ -r "$CMDLINE_FILE" ]] || die "cmdline file is not readable: $CMDLINE_FILE"
    CMDLINE="$(<"$CMDLINE_FILE")"
  fi
  [[ -n "$CMDLINE" ]] || die "kernel cmdline is empty"
  KERNEL="$(readlink -f "$KERNEL")"
  INITRD="$(readlink -f "$INITRD")"
  INIT="$(readlink -f "$INIT")"
  if [[ " $CMDLINE " != *" init="* ]]; then
    CMDLINE+=" init=${INIT}"
  fi
}

function ensure_vfio_access() {
  local bdf='' driver='' iommu_group='' group_dev='' member='' seen_groups=''
  [[ "${#VFIO_BDFS[@]}" -eq 0 ]] && return 0
  [[ -r /dev/vfio/vfio && -w /dev/vfio/vfio ]] || die "current user cannot access /dev/vfio/vfio"
  for bdf in "${VFIO_BDFS[@]}"; do
    [[ -d "/sys/bus/pci/devices/${bdf}" ]] || die "PCI device not found: ${bdf}"
    driver="$(basename "$(readlink -f "/sys/bus/pci/devices/${bdf}/driver")")"
    [[ "$driver" == "vfio-pci" ]] || die "${bdf} is not bound to vfio-pci"
    iommu_group="$(basename "$(readlink -f "/sys/bus/pci/devices/${bdf}/iommu_group")")"
    group_dev="/dev/vfio/${iommu_group}"
    [[ -r "$group_dev" && -w "$group_dev" ]] || die "current user cannot access ${group_dev}"
    case " ${seen_groups} " in
      *" ${iommu_group} "*) continue ;;
    esac
    seen_groups="${seen_groups} ${iommu_group}"
    for member in "/sys/kernel/iommu_groups/${iommu_group}/devices/"*; do
      driver="$(basename "$(readlink -f "${member}/driver")")"
      [[ "$driver" == "vfio-pci" ]] || die "IOMMU group ${iommu_group} member ${member##*/} is not bound to vfio-pci"
      case " ${VFIO_BDFS[*]} " in
        *" ${member##*/} "*) ;;
        *) die "IOMMU group ${iommu_group} member ${member##*/} is not listed via --vfio-bdf" ;;
      esac
    done
  done
}

function preflight() {
  require_command virsh
  require_command qemu-system-x86_64
  require_command virtiofsd
  require_command ssh
  require_command ss
  VIRTIOFSD_BIN="$(command -v virtiofsd)"
  virsh -c "$URI" uri >/dev/null
  virsh -c "$URI" capabilities >/dev/null
  [[ -e /dev/kvm ]] || die "/dev/kvm is not present on the host"
  [[ -r /dev/kvm ]] || die "/dev/kvm is not readable by current user"
  [[ -e /dev/fuse ]] || die "/dev/fuse is not present on the host"
  [[ -r /dev/fuse && -w /dev/fuse ]] || die "/dev/fuse is not accessible by current user"
  check_kvm_usable
  check_nested_enabled
  check_port_free
  [[ -d "$NIX_DIR" ]] || die "nix directory does not exist: $NIX_DIR"
  ensure_vfio_access
}

function make_state_dirs() {
  mkdir -p "$STATE_ROOT" "$PERSISTENT_DIR" "$WORKSPACE_DIR" "${PERSISTENT_DIR}/home/${SSH_USER}"
}

function share_socket_path() {
  printf '%s/fs%s-%s.sock\n' "$STATE_ROOT" "$1" "$2"
}

function share_log_path() {
  printf '%s/fs%s-%s.log\n' "$STATE_ROOT" "$1" "$2"
}

function share_pid_path() {
  printf '%s/fs%s-%s.pid\n' "$STATE_ROOT" "$1" "$2"
}

function wait_for_share_socket() {
  local socket_path="$1" pid="$2" log_path="$3" attempts='0'
  while (( attempts < 50 )); do
    [[ -S "$socket_path" ]] && return 0
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      [[ ! -r "$log_path" ]] || tail -n 50 "$log_path" >&2 || true
      die "virtiofsd exited before socket became ready: ${socket_path}"
    fi
    attempts=$((attempts + 1))
    sleep 0.1
  done
  die "timed out waiting for virtiofsd socket: ${socket_path}"
}

function cleanup_share_daemons() {
  local pidfile='' pid=''
  for pidfile in \
    "$(share_pid_path 0 nix)" \
    "$(share_pid_path 1 persistent)" \
    "$(share_pid_path 2 workspace)"
  do
    [[ -r "$pidfile" ]] || continue
    pid="$(<"$pidfile")"
    [[ -n "$pid" ]] && kill "$pid" >/dev/null 2>&1 || true
    rm -f "$pidfile"
  done
  rm -f \
    "$(share_socket_path 0 nix)" \
    "$(share_socket_path 1 persistent)" \
    "$(share_socket_path 2 workspace)"
}

function cleanup_stale_share_daemons() {
  domain_running && return 0
  cleanup_share_daemons
}

function start_share() {
  local index="$1" tag="$2" source_dir="$3" readonly="$4"
  local socket_path='' log_path='' pid_path='' pid=''
  local -a cmd=()
  socket_path="$(share_socket_path "$index" "$tag")"
  log_path="$(share_log_path "$index" "$tag")"
  pid_path="$(share_pid_path "$index" "$tag")"
  rm -f "$socket_path" "$log_path" "$pid_path"
  cmd=(
    "$VIRTIOFSD_BIN"
    --socket-path "$socket_path"
    --shared-dir "$source_dir"
    --sandbox "$VIRTIOFSD_SANDBOX"
    --log-level "$VIRTIOFSD_LOG_LEVEL"
  )
  if [[ "$readonly" == "1" ]]; then
    cmd+=(--readonly)
  fi
  nohup "${cmd[@]}" </dev/null >"$log_path" 2>&1 &
  pid="$!"
  printf '%s\n' "$pid" >"$pid_path"
  wait_for_share_socket "$socket_path" "$pid" "$log_path"
  printf '%s\n' "$socket_path"
}

function start_share_daemons() {
  FS_NIX_SOCKET="$(start_share 0 nix "$NIX_DIR" 1)"
  FS_PERSISTENT_SOCKET="$(start_share 1 persistent "$PERSISTENT_DIR" 0)"
  FS_WORKSPACE_SOCKET="$(start_share 2 workspace "$WORKSPACE_DIR" 0)"
}

function append_hostdev_xml() {
  local bdf='' domain='' bus='' slot='' function=''
  for bdf in "${VFIO_BDFS[@]}"; do
    IFS=':.' read -r domain bus slot function <<<"$bdf"
    cat <<EOF
    <hostdev mode='subsystem' type='pci' managed='no'>
      <driver name='vfio'/>
      <source>
        <address domain='0x${domain}' bus='0x${bus}' slot='0x${slot}' function='0x${function}'/>
      </source>
    </hostdev>
EOF
  done
}

function write_domain_xml() {
  local hostdev_xml=''
  if [[ "${#VFIO_BDFS[@]}" -gt 0 ]]; then
    hostdev_xml="$(append_hostdev_xml)"
  fi
  cat >"$DOMAIN_XML" <<EOF
<domain type='kvm' xmlns:qemu='http://libvirt.org/schemas/domain/qemu/1.0'>
  <name>${NAME}</name>
  <memory unit='MiB'>${MEMORY_MIB}</memory>
  <currentMemory unit='MiB'>${MEMORY_MIB}</currentMemory>
  <vcpu placement='static'>${VCPUS}</vcpu>
  <os>
    <type arch='x86_64' machine='q35'>hvm</type>
    <kernel>${KERNEL}</kernel>
    <initrd>${INITRD}</initrd>
    <cmdline>${CMDLINE}</cmdline>
  </os>
  <cpu mode='host-passthrough' migratable='off'/>
  <memoryBacking>
    <source type='memfd'/>
    <access mode='shared'/>
  </memoryBacking>
  <features>
    <acpi/>
    <apic/>
  </features>
  <devices>
    <filesystem type='mount' accessmode='passthrough'>
      <driver type='virtiofs' queue='1024'/>
      <source socket='${FS_NIX_SOCKET}'/>
      <target dir='nix'/>
      <readonly/>
    </filesystem>
    <filesystem type='mount' accessmode='passthrough'>
      <driver type='virtiofs' queue='1024'/>
      <source socket='${FS_PERSISTENT_SOCKET}'/>
      <target dir='persistent'/>
    </filesystem>
    <filesystem type='mount' accessmode='passthrough'>
      <driver type='virtiofs' queue='1024'/>
      <source socket='${FS_WORKSPACE_SOCKET}'/>
      <target dir='workspace'/>
    </filesystem>
    <channel type='unix'>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
    </channel>
    <serial type='pty'>
      <target port='0'/>
    </serial>
    <console type='pty'>
      <target type='serial' port='0'/>
    </console>
${hostdev_xml}
  </devices>
  <qemu:commandline>
    <qemu:arg value='-netdev'/>
    <qemu:arg value='user,id=hostnet0,hostfwd=tcp:127.0.0.1:${SSH_PORT}-:22'/>
    <qemu:arg value='-device'/>
    <qemu:arg value='virtio-net-pci,netdev=hostnet0,id=net0,bus=pcie.0,addr=0x2'/>
  </qemu:commandline>
</domain>
EOF
}

function ssh_base() {
  printf '%s\0' ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=2 -p "$SSH_PORT" "${SSH_USER}@127.0.0.1"
}

function ssh_run() {
  local -a cmd=()
  mapfile -d '' -t cmd < <(ssh_base)
  if [[ "${#SSH_ARGS[@]}" -eq 0 ]]; then
    "${cmd[@]}"
  else
    "${cmd[@]}" "${SSH_ARGS[@]}"
  fi
}

function wait_for_guest() {
  local deadline='' state=''
  deadline=$((SECONDS + SSH_WAIT_SECS))
  while (( SECONDS < deadline )); do
    if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2 \
      -p "$SSH_PORT" "${SSH_USER}@127.0.0.1" true >/dev/null 2>&1; then
      ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2 \
        -p "$SSH_PORT" "${SSH_USER}@127.0.0.1" \
        "state=\$(systemctl is-system-running --wait || true)
         [[ \$state == running || \$state == degraded ]]
         systemctl is-active sshd libvirtd >/dev/null
         grep -Eq '(vmx|svm)' /proc/cpuinfo
         test -e /dev/kvm" >/dev/null && return 0
    fi
    if ! domain_running; then
      state="$(domstate)"
      die "domain stopped before becoming ready: ${state:-unknown}"
    fi
    sleep 2
  done
  return 1
}

function up() {
  mkdir -p "$STATE_ROOT"
  exec 9>"$LOCKFILE"
  flock -n 9 || die "another experiment is using ${LOCKFILE}"
  ensure_artifacts
  preflight
  domain_running && die "domain ${NAME} is already running"
  cleanup_stale_share_daemons
  make_state_dirs
  start_share_daemons
  write_domain_xml
  if ! virsh -c "$URI" create "$DOMAIN_XML" >/dev/null; then
    cleanup_share_daemons
    die "failed to start domain ${NAME}"
  fi
  if ! wait_for_guest; then
    if [[ "$KEEP_FAILED_VM" != "1" ]]; then
      virsh -c "$URI" destroy "$NAME" >/dev/null 2>&1 || true
      cleanup_share_daemons
    fi
    die "guest did not become ready within ${SSH_WAIT_SECS} seconds"
  fi
  printf 'guest ready: ssh -p %s %s@127.0.0.1\n' "$SSH_PORT" "$SSH_USER"
  printf 'console: virsh -c %s console %s\n' "$URI" "$NAME"
  [[ "$ATTACH_CONSOLE" == "1" ]] && exec virsh -c "$URI" console "$NAME"
}

function down() {
  domain_running || die "domain ${NAME} is not running"
  virsh -c "$URI" shutdown "$NAME"
}

function destroy_domain() {
  domain_running || die "domain ${NAME} is not running"
  virsh -c "$URI" destroy "$NAME"
  cleanup_share_daemons
}

function status() {
  local state
  state="$(domstate)"
  [[ -n "$state" ]] || die "domain ${NAME} does not exist"
  printf '%s\n' "$state"
}

function console() {
  domain_running || die "domain ${NAME} is not running"
  exec virsh -c "$URI" console "$NAME"
}

function logs() {
  if [[ "${#SSH_ARGS[@]}" -eq 0 ]]; then
    SSH_ARGS=(-en1000)
  fi
  SSH_ARGS=(journalctl "${SSH_ARGS[@]}")
  ssh_run
}

case "$ACTION" in
  up) up ;;
  down) down ;;
  destroy) destroy_domain ;;
  status) status ;;
  console) console ;;
  ssh) ssh_run ;;
  logs) logs ;;
  *) die "unknown action: $ACTION" ;;
esac
