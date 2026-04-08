#!/usr/bin/env bash
# Required packages: curl, tar, jq, bubblewrap, util-linux, passt.
set -ev

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
  bwrap --bind "$SYSROOT" / --ro-bind /etc/resolv.conf /etc/resolv.conf --proc /proc --dev /dev \
    /nix/var/nix/profiles/default/bin/nix --extra-experimental-features 'nix-command flakes' \
      build /etc/nixos#nixosConfigurations.agenthouse.config.system.build.toplevel \
      --out-link /nix/var/nix/profiles/system
}

# Start NixOS in container, setup firewall, etc. Requires: unshare, passt, bubblewarp.
function chroot_to() {
  local SYSROOT="$1"; shift
  unshare --map-auto --map-root-user \
    pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
          --tcp-ports 2222:22 --udp-ports none --netns-only \
      bwrap --die-with-parent --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
            --bind "$SYSROOT" / --dev /dev --proc /proc --tmpfs /run --tmpfs /tmp \
            --ro-bind /sys /sys --bind /sys/fs/cgroup /sys/fs/cgroup \
            --ro-bind /etc/resolv.conf /etc/resolv.conf \
            --clearenv \
        -- /nix/var/nix/profiles/default/bin/bash \
           --init-file /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh -i "$@"
}

function chroot2() {
  local SYSROOT="$1"; shift
  unshare --map-auto --map-root-user \
    pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
          --tcp-ports 2222:22 --udp-ports none --netns-only \
      bwrap --die-with-parent --unshare-pid --unshare-ipc --unshare-uts\
            --bind "$SYSROOT" / --dev /dev --proc /proc --tmpfs /run --tmpfs /tmp \
            --ro-bind /sys /sys --bind /sys/fs/cgroup /sys/fs/cgroup \
            --ro-bind /etc/resolv.conf /etc/resolv.conf \
            --clearenv \
        --as-pid-1 /nix/var/nix/profiles/system/init
}

function chroot3() {
  local SYSROOT="$1"; shift
  unshare --user --map-auto --map-root-user --ipc --mount --pid --cgroup --time \
    pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
          --tcp-ports 2222:22 --udp-ports none --netns-only \
      bwrap --die-with-parent --unshare-pid --unshare-ipc --unshare-uts\
            --bind "$SYSROOT" / --dev /dev --proc /proc --tmpfs /run --tmpfs /tmp \
            --ro-bind /sys /sys --bind /sys/fs/cgroup /sys/fs/cgroup \
            --ro-bind /etc/resolv.conf /etc/resolv.conf \
            --clearenv \
        --as-pid-1 /nix/var/nix/profiles/system/init
}

function main() {
  local SYSROOT="${1:-sysroot}" PID=
  local RUNTIME_DIR="$SYSROOT/.host" PIDFILE="$SYSROOT/.host/container.pid"
  [[ -d "$SYSROOT/nix/store" ]] || fetch_nixos_dockerhub "$SYSROOT"
  [[ -e "$SYSROOT/nix/var/nix/profiles/system" ]] || install_nixos "$SYSROOT"
  [[ -f "$PIDFILE" ]] && PID="$(<"$PIDFILE")"
  if [[ -z "$PID" ]] || ! kill -0 "$PID" 2>/dev/null; then
    install -d "$RUNTIME_DIR"
    chroot2 "$SYSROOT" >"$RUNTIME_DIR/console.log" 2>&1 &
    PID=$!
    echo "$PID" >"$PIDFILE"
  fi
  nsenter -t "$PID" -a /run/current-system/sw/bin/bash -l
}

main "$@"
