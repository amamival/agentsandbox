#!/usr/bin/env bash
# Required packages: curl, tar, jq, bubblewrap, util-linux, passt, sudo.
set -euo pipefail

APP_DIR="/sandbox"
SRC_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
SYSROOT="sysroot"
PERSISTENT="persistent"
PIDFILE="sysroot.pid"

function die() { echo "$0: $*" >&2; exit 1; }

function usage() {
  cat <<'EOF'
usage: ./sandbox.sh [help|build|up|down|kill|pause|unpause|exec|logs|ssh|add|delete|mount|unmount|--] [args ...]

  help     show this help
  build    prepare sandbox state and rebuild the NixOS system
  up       rebuild and start the sandbox; fail if already running
  down     graceful shutdown (SIGRTMIN+3 to guest systemd)
  kill     send TERM to all processes in the sandbox pid namespace
  pause    send STOP to all processes in the sandbox pid namespace
  unpause  send CONT to all processes in the sandbox pid namespace
  exec     attach to the running sandbox, or run a command
  logs     run journalctl in the sandbox; default args: -en1000
  ssh      ssh to vscode@127.0.0.1 -p 2222; pass args through to ssh
  add      register host directories in /sandbox/mounts and mount them now
  delete   unmount directories and remove them from /sandbox/mounts
  mount    mount all directories listed in /sandbox/mounts
  unmount  unmount all directories listed in /sandbox/mounts

With no subcommand:
  - if the sandbox is running, attach
  - if the sandbox is stopped, behave like build then up
Use `exec [--] cmd ...` or `-- cmd ...` to run a command via exec.
EOF
}

# Run as ID-mapped isolated root user within $APP_DIR.
function nssudo() {
  unshare --map-auto --setuid=0 --setgid=0 --wd "$APP_DIR" "$@"
}

function prepare() {
  local SYSROOT="$1" PERSISTENT="$2" PIDFILE="$3"
  cat >&2 <<'EOF'
This script uses a rootless container runtime, but it still needs sudo for host-side setup.

- `unshare --map-auto --setuid=0 --setgid=0` creates an isolated root whose host
  identity is a subuid/subgid range, not host root.
- bubblewrap canonicalizes bind-mount source paths on the host. If those paths
  live under a locked-down `$HOME`, that isolated root cannot walk the host path
  and `--bind` fails before the container starts.
- moving the app state to `/sandbox` fixes path reachability, but `/sandbox`
  itself must be created and owned in the host namespace.
- creating root-owned paths under `/`, preparing bind mount sources, and doing
  host-side id-mapped mount plumbing all require privileges in the initial mount
  namespace. Namespace root created by `unshare` does not have those privileges.

sudo is therefore limited to preparing `/sandbox` and related host mounts.
The NixOS payload still runs rootlessly inside its own user namespace.
EOF
  sudo install -d -m 777 "$APP_DIR"
  nssudo install -d "$SYSROOT" "$PERSISTENT"
  nssudo install -m 644 /dev/null "$PIDFILE"
  sudo install -d -m 755 "$APP_DIR"
}

# Download nixos/nix image from Docker Hub.
function fetch_nixos_dockerhub() {
  local SYSROOT="$1"
  local REPO=nixos/nix TAG=latest ARCH=amd64 OS=linux REGISTRY_ENDPOINT="https://registry-1.docker.io/v2"
  TOKEN="$(curl "https://auth.docker.io/token?service=registry.docker.io&scope=repository:$REPO:pull" | jq -r .token)"
  echo "TOKEN=$TOKEN"
  MANIFESTS="$(curl -H "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/manifests/$TAG")"
  echo "MANIFESTS=$MANIFESTS"
  MANIFEST_DIGEST="$(jq -r ".manifests[] | select(.platform.architecture == \"$ARCH\" and .platform.os == \"$OS\") | .digest" <<<"$MANIFESTS")"
  echo "MANIFEST_DIGEST=$MANIFEST_DIGEST"
  MANIFEST="$(curl -H "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/manifests/$MANIFEST_DIGEST")"
  echo "MANIFEST=$MANIFEST"
  BLOBSUMS="$(jq -r '.layers[].digest' <<<"$MANIFEST")"
  echo "BLOBSUMS=$BLOBSUMS"
  while read -r BLOBSUM; do
    curl -IH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM"
    curl -LH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM" | nssudo tar zxf - -C "$SYSROOT"
  done <<<"$BLOBSUMS"
}

# Build NixOS system in container.
function install_nixos() {
  local SYSROOT="$1"
  nssudo install -d "$SYSROOT/etc/nixos"
  cp "$SRC_DIR"/{flake,configuration}.nix /tmp
  nssudo install -m 644 /tmp/{flake,configuration}.nix "$SYSROOT/etc/nixos/"
  nssudo rm -f "$SYSROOT/nix/var/nix/profiles/system"
  # Python's _multiprocessing.SemLock expects a writable 1777 /dev/shm.
  nssudo bwrap --bind "$SYSROOT" / --ro-bind /etc/resolv.conf /etc/resolv.conf \
               --proc /proc --dev /dev --perms 1777 --tmpfs /dev/shm \
    /nix/var/nix/profiles/default/bin/nix --extra-experimental-features 'nix-command flakes' \
      build /etc/nixos#nixosConfigurations.agenthouse.config.system.build.toplevel \
      --out-link /nix/var/nix/profiles/system
}

function ensure_system() {
  local SYSROOT="$1" PERSISTENT="$2" PIDFILE="$3"
  [[ -f "$APP_DIR/$PIDFILE" ]] || prepare "$SYSROOT" "$PERSISTENT" "$PIDFILE"
  [[ -d "$APP_DIR/$SYSROOT/nix/store" ]] || fetch_nixos_dockerhub "$SYSROOT"
  install_nixos "$SYSROOT"
}

# Start NixOS in container, setup firewall, etc.
function start_container() {
  local SYSROOT="$1" PERSISTENT="$2" PIDFILE="$3" CURRENT_CGROUP
  # Let the id-mapped container systemd open the host tty via bwrap's /dev/console bind.
  chmod o+rw "$(tty)" # nssudo --map-current-user chown 0:0 "$(tty)" <- won't work.
  # Change owner of cgroup and bind mount into container directly.
  CURRENT_CGROUP="/sys/fs/cgroup$(awk -F: '$1==0{print $3}' /proc/self/cgroup)"
  nssudo --map-current-user chown -R 0:0 "$CURRENT_CGROUP"
  nssudo /bin/sh -c 'echo $$ > "'"$PIDFILE"'"; exec "$@"' _ \
    pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
          --tcp-ports 2222:22 --netns-only \
      bwrap --die-with-parent --unshare-pid --unshare-ipc --unshare-uts \
            --tmpfs / --dev /dev --proc /proc --ro-bind /sys /sys \
            --unshare-cgroup --bind "$CURRENT_CGROUP" /sys/fs/cgroup \
            --bind "$SYSROOT/nix" /nix --bind "$PERSISTENT" /persistent \
            --ro-bind /etc/resolv.conf /etc/resolv.conf \
            --clearenv --new-session --as-pid-1 \
        /nix/var/nix/profiles/system/init
}

function running_pid() {
  local PID="$(<"$APP_DIR/$PIDFILE")"
  if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then
    pgrep -o --ns "$PID" --nslist user -f /run/current-system/systemd/lib/systemd/systemd
  else false
  fi
}
function map_inner_1000() {
  awk '1000 >= $1 && 1000 < ($1 + $3) { print $2 + 1000 - $1; found = 1; exit } END { exit !found }' "$1" ||
    die "inner $2 1000 is not mapped in $1"
}
function require_running_pid() { running_pid || die "sandbox is not running"; }
function sandbox_exec() { env -i "TERM=${TERM:-xterm-256color}" nsenter -t "$1" -U -m -n -p -i -u ${2:+--} "${@:2}"; }
function send_signal() { pkill -"$1" --ns "$(require_running_pid)" --nslist pid -f . 2>/dev/null || true; }
function mount_workspace() {
  local src="$1" dst="$APP_DIR/$PERSISTENT/workspace/${1##*/}" cur uid="$2" gid="$3"
  if mountpoint -q "$dst"; then
    cur="$(findmnt -n -o SOURCE --target "$dst" || true)"
    [[ "$cur" == "$src" ]] && { echo "/workspace/${src##*/} is already mounted"; return; }
    die "$dst is already mounted from $cur"
  fi
  sudo sh -eu -c 'install -d "$1" && mount --bind --mkdir --map-users "$2:$3:1" --map-groups "$4:$5:1" "$6" "$7"' sh \
    "$(dirname "$dst")" "$uid" "$UID" "$gid" "$(id -g)" "$src" "$dst"
  echo "$src -> /workspace/${src##*/}"
}
function unmount_workspace() {
  local src="$1" dst="$APP_DIR/$PERSISTENT/workspace/${1##*/}" cur
  mountpoint -q "$dst" || return
  cur="$(findmnt -n -o SOURCE --target "$dst" || true)"
  [[ -z "$cur" || "$cur" == "$src" ]] || die "$dst is mounted from $cur"
  sudo umount "$dst"
  echo "/workspace/${src##*/} unmounted"
}

function main() {
  local CMD="${1:-}" PID= src uid gid tmp
  local -a lines=()
  case "$CMD" in
    help|-h|--help) usage ;;
    build) ensure_system "$SYSROOT" "$PERSISTENT" "$PIDFILE" ;;
    up|--|"") [[ -n "$CMD" ]] && shift
      if PID="$(running_pid 2>/dev/null)"; then
        if [[ "$CMD" != up ]];
          then sandbox_exec "$PID" "$@";
          else die "sandbox is already running at pid $PID";
        fi
      else
        ensure_system "$SYSROOT" "$PERSISTENT" "$PIDFILE"
        start_container "$SYSROOT" "$PERSISTENT" "$PIDFILE"
      fi
      ;;
    down) PID="$(running_pid 2>/dev/null || true)"; [[ -z "$PID" ]] || kill -s SIGRTMIN+3 "$PID" ;;
    kill) send_signal TERM ;;
    pause) send_signal STOP ;;
    unpause) send_signal CONT ;;
    exec) shift; [[ "${1:-}" == "--" ]] && shift;
      sandbox_exec "$(require_running_pid)" "$@" ;;
    logs) shift; sandbox_exec "$(require_running_pid)" journalctl "${@:--en1000}" ;;
    ssh) shift; require_running_pid >/dev/null; exec ssh -p 2222 vscode@127.0.0.1 "$@" ;;
    add)
      shift
      [[ "$#" -gt 0 ]] || die "add requires at least one path"
      PID="$(require_running_pid)"; uid="$(map_inner_1000 "/proc/$PID/uid_map" UID)"; gid="$(map_inner_1000 "/proc/$PID/gid_map" GID)"
      sudo install -D -m 644 /dev/null "$APP_DIR/mounts"
      for src in "$@"; do
        src="$(realpath -e "$src")"
        [[ -d "$src" ]] || die "only directories are supported: $src"
        grep -Fxq -- "$src" "$APP_DIR/mounts" || printf '%s\n' "$src" | sudo tee -a "$APP_DIR/mounts" >/dev/null
        mount_workspace "$src" "$uid" "$gid"
      done
      ;;
    delete)
      shift
      [[ "$#" -gt 0 ]] || die "delete requires at least one path"
      sudo install -D -m 644 /dev/null "$APP_DIR/mounts"
      for src in "$@"; do
        src="$(realpath -m "$src")"
        tmp="$(mktemp)"
        grep -Fxv -- "$src" "$APP_DIR/mounts" >"$tmp" || true
        sudo install -m 644 "$tmp" "$APP_DIR/mounts"
        rm -f "$tmp"
        unmount_workspace "$src"
      done
      ;;
    mount)
      shift
      [[ -r "$APP_DIR/mounts" ]] || return
      PID="$(require_running_pid)"; uid="$(map_inner_1000 "/proc/$PID/uid_map" UID)"; gid="$(map_inner_1000 "/proc/$PID/gid_map" GID)"
      while IFS= read -r src; do [[ -n "$src" ]] && mount_workspace "$src" "$uid" "$gid"; done < "$APP_DIR/mounts"
      ;;
    unmount)
      shift
      [[ -r "$APP_DIR/mounts" ]] || return
      mapfile -t lines < "$APP_DIR/mounts"
      for ((i=${#lines[@]}-1; i>=0; i--)); do [[ -n "${lines[i]}" ]] && unmount_workspace "${lines[i]}"; done
      ;;
    *) die "unknown subcommand: $CMD" ;;
  esac
}

main "$@"
