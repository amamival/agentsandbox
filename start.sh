#!/usr/bin/env bash
# Required packages: curl, tar, jq, bubblewrap, util-linux, passt.
set -e

# Download nixos/nix image from Docker Hub. Requires: curl, tar, jq.
function fetch_nixos_dockerhub() {
  local TARGET_DIR="$1"
  [[ -z "$TARGET_DIR" ]] && echo "$0 <NEW_SYSROOT>" && exit
  mkdir -p "$TARGET_DIR"
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
    curl -LH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM" | tar zxf - -C "$TARGET_DIR"
  done <<<"$BLOBSUMS"
}

# Build NixOS system in container. Requires: bwrap.
function install_nixos() {
  local SYSROOT="$1"
  install -d "$SYSROOT/etc/nixos"
  install -m 0644 flake.nix configuration.nix "$SYSROOT/etc/nixos/"
  rm -f "$SYSROOT/nix/var/nix/profiles/system"
  bwrap --bind "$SYSROOT" / --ro-bind /etc/resolv.conf /etc/resolv.conf --proc /proc --dev /dev \
    /nix/var/nix/profiles/default/bin/nix --extra-experimental-features 'nix-command flakes' \
      build /etc/nixos#nixosConfigurations.agenthouse.config.system.build.toplevel \
      --out-link /nix/var/nix/profiles/system
}

# Start NixOS in container, setup firewall, etc. Requires: unshare, passt, bubblewarp.
function start_container() {
  local SYSROOT="$1" PERSISTENT="$2" PIDFILE="$3"
  unshare --map-auto --map-root-user \
    /bin/sh -c 'echo $$ > "'"$PIDFILE"'"; exec "$@"' _ \
      pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
            --tcp-ports 2222:22 --udp-ports none --netns-only \
        bwrap --die-with-parent --unshare-pid --unshare-ipc --unshare-uts \
              --tmpfs / --bind "$SYSROOT/nix" /nix --dev /dev --proc /proc \
              --bind "$PERSISTENT" /persistent \
              --ro-bind /sys /sys --bind /sys/fs/cgroup /sys/fs/cgroup \
              --ro-bind /etc/resolv.conf /etc/resolv.conf \
              --clearenv --as-pid-1 /nix/var/nix/profiles/system/init
}

# Attach to the running container.
function attach() {
  local PID="$1"
  PID="$(pgrep --ns "$PID" --nslist user -f /run/current-system/systemd/lib/systemd/systemd)"
  env -i "TERM=$TERM" nsenter -t "$PID" -U -m -n -p -i -u
}

function main() {
  local XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
  local XDG_STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"
  local XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  local SYSROOT="$XDG_DATA_HOME/agentsandbox/sysroot" PID=
  local PERSISTENT="$XDG_STATE_HOME/agentsandbox/persistent"
  local PIDFILE="$XDG_RUNTIME_DIR/agentsandbox.pid"
  install -d "$PERSISTENT" "$XDG_RUNTIME_DIR"
  [[ -d "$SYSROOT/nix/store" ]] || fetch_nixos_dockerhub "$SYSROOT"
  [[ -f "$PIDFILE" ]] && PID="$(<"$PIDFILE")"
  if [[ -z "$PID" ]] || ! kill -0 "$PID" 2>/dev/null; then
    install_nixos "$SYSROOT"
    start_container "$SYSROOT" "$PERSISTENT" "$PIDFILE"
  else
    attach "$PID"
  fi
}

main "$@"
