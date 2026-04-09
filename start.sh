#!/usr/bin/env bash
# Required packages: curl, tar, jq, bubblewrap, util-linux, passt.
set -e

XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_DIR="$XDG_DATA_HOME/agentsandbox"
SRC_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"

# Run as ID-mapped 'unprevileged' root user within $APP_DIR.
function nssudo() {
  # Independent idrange unused in host cannot access $HOME.
  # To avoid this limitation, use path relative to $APP_DIR.
  unshare --map-auto --setuid=0 --setgid=0 --wd "$APP_DIR" "$@"
}

# Download nixos/nix image from Docker Hub. Requires: curl, tar, jq.
function fetch_nixos_dockerhub() {
  local SYSROOT="$1" STATEDIR="$2"
  install -d -m 777 "$APP_DIR"
  nssudo install -d "$SYSROOT" "$STATEDIR"
  install -d -m 755 "$APP_DIR"
  local REPO=nixos/nix TAG=latest ARCH=amd64 OS=linux REGISTRY_ENDPOINT="https://registry-1.docker.io/v2"
  TOKEN="$(curl "https://auth.docker.io/token?service=registry.docker.io&scope=repository:$REPO:pull" | jq -r .token)"
  echo "TOKEN=$TOKEN"
  MANIFESTS=$(curl -H "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/manifests/$TAG")
  echo "MANIFESTS=$MANIFESTS"
  MANIFEST_DIGEST=$(jq -r ".manifests[] | select(.platform.architecture == \"$ARCH\" and .platform.os == \"$OS\") | .digest" <<<"$MANIFESTS")
  echo "MANIFEST_DIGEST=$MANIFEST_DIGEST"
  MANIFEST=$(curl -H "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/manifests/$MANIFEST_DIGEST")
  echo "MANIFEST=$MANIFEST"
  BLOBSUMS="$(jq -r '.layers[].digest' <<<"$MANIFEST")"
  echo "BLOBSUMS=$BLOBSUMS"
  while read -r BLOBSUM; do
    curl -IH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM";
    curl -LH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM" | nssudo tar zxf - -C "$SYSROOT"
  done <<<"$BLOBSUMS"
}

# Build NixOS system in container. Requires: bwrap.
function install_nixos() {
  local SYSROOT="$1"
  nssudo install -d "$SYSROOT/etc/nixos"
  cp "$SRC_DIR"/*.nix "$APP_DIR"
  nssudo install -m 644 flake.nix configuration.nix "$SYSROOT/etc/nixos/"
  nssudo rm -f "$SYSROOT/nix/var/nix/profiles/system"
  nssudo bwrap --bind "$SYSROOT" / --ro-bind /etc/resolv.conf /etc/resolv.conf --proc /proc --dev /dev \
    /nix/var/nix/profiles/default/bin/nix --extra-experimental-features 'nix-command flakes' \
      build /etc/nixos#nixosConfigurations.agenthouse.config.system.build.toplevel \
      --out-link /nix/var/nix/profiles/system
}

# Start NixOS in container, setup firewall, etc. Requires: unshare, passt, bubblewarp.
function start_container() {
  local SYSROOT="$1" PERSISTENT="$2" PIDFILE="$3"
  install -m 666 /dev/null "$APP_DIR/$PIDFILE"
  nssudo /bin/sh -c 'echo $$ > "'"$PIDFILE"'"; exec "$@"' _ \
    pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
          --tcp-ports 2222:22 --udp-ports none --netns-only \
      bwrap --die-with-parent --unshare-pid --unshare-ipc --unshare-uts \
            --tmpfs / --bind "$SYSROOT/nix" /nix --dev /dev --proc /proc \
            --bind "$PERSISTENT" /persistent \
            --ro-bind /sys /sys --bind /sys/fs/cgroup /sys/fs/cgroup \
            --ro-bind /etc/resolv.conf /etc/resolv.conf \
            --clearenv --as-pid-1
        /nix/var/nix/profiles/system/init
}

# Attach to the running container.
function attach() {
  local PID="$1"
  PID="$(pgrep --ns "$PID" --nslist user -f /run/current-system/systemd/lib/systemd/systemd)"
  env -i "TERM=$TERM" nsenter -t "$PID" -U -m -n -p -i -u
}

function main() {
  local SYSROOT="sysroot" STATEDIR="persistent" PIDFILE="sysroot.pid" PID=
  [[ -d "$APP_DIR/$SYSROOT/nix/store" ]] || fetch_nixos_dockerhub "$SYSROOT" "$STATEDIR"
  [[ -f "$APP_DIR/$PIDFILE" ]] && PID="$(<"$APP_DIR/$PIDFILE")"
  if [[ -z "$PID" ]] || ! kill -0 "$PID" 2>/dev/null; then
    install_nixos "$SYSROOT"
    start_container "$SYSROOT" "$PERSISTENT" "$PIDFILE"
  else
    attach "$PID"
  fi
}

main "$@"
