#!/usr/bin/env bash
# Required packages: curl, tar, jq, bubblewrap, util-linux, passt, sudo.
set -e

APP_DIR="/sandbox"
SRC_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"

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
  cp {flake,configuration}.nix /tmp
  nssudo install -m 644 /tmp/{flake,configuration}.nix "$SYSROOT/etc/nixos/"
  nssudo rm -f "$SYSROOT/nix/var/nix/profiles/system"
  # Python's _multiprocessing.SemLock expects a writable 1777 /dev/shm.
  nssudo bwrap --bind "$SYSROOT" / --ro-bind /etc/resolv.conf /etc/resolv.conf \
               --proc /proc --dev /dev --perms 1777 --tmpfs /dev/shm \
    /nix/var/nix/profiles/default/bin/nix --extra-experimental-features 'nix-command flakes' \
      build /etc/nixos#nixosConfigurations.agenthouse.config.system.build.toplevel \
      --out-link /nix/var/nix/profiles/system
}

# Start NixOS in container, setup firewall, etc.
function start_container() {
  local SYSROOT="$1" PERSISTENT="$2" PIDFILE="$3"
  # Let the id-mapped container systemd open the host tty via bwrap's /dev/console bind.
  chmod o+rw "$(tty)" # nssudo --map-current-user chown 0:0 "$(tty)" <- won't work.
  # Change owner of cgroup and bind mount into container directly.
  local CURRENT_CGROUP="/sys/fs/cgroup$(awk -F: '$1==0{print $3}' /proc/self/cgroup)"
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

# Attach to the running container.
function attach() {
  local PID="$1"
  PID="$(pgrep --ns "$PID" --nslist user -f /run/current-system/systemd/lib/systemd/systemd)"
  env -i "TERM=$TERM" nsenter -t "$PID" -U -m -n -p -i -u
}

function main() {
  local SYSROOT="sysroot" PERSISTENT="persistent" PIDFILE="sysroot.pid" PID=
  [[ -f "$APP_DIR/$PIDFILE" ]] || prepare "$SYSROOT" "$PERSISTENT" "$PIDFILE" \
    && PID="$(<"$APP_DIR/$PIDFILE")"
  [[ -d "$APP_DIR/$SYSROOT/nix/store" ]] || fetch_nixos_dockerhub "$SYSROOT"
  if [[ -z "$PID" ]] || ! kill -0 "$PID" 2>/dev/null; then
    install_nixos "$SYSROOT"
    start_container "$SYSROOT" "$PERSISTENT" "$PIDFILE"
  else
    attach "$PID"
  fi
}

main "$@"
