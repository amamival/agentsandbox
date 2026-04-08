#!/usr/bin/env bash
set -e

# Download nixos/nix image from Docker Hub. Requires: curl, tar, jq.
function fetch_nixos_dockerhub() {
  TARGET_DIR="$1"
  [[ -z "$TARGET_DIR" ]] && echo "$0 <NEW_SYSROOT>" && exit
  mkdir -p "$TARGET_DIR"
  REPO=nixos/nix TAG=latest ARCH=amd64 OS=linux REGISTRY_ENDPOINT="https://registry-1.docker.io/v2"
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
  while read BLOBSUM; do
    curl -IH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM";
    curl -LH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM" | tar zxf - -C "$TARGET_DIR"
  done <<<"$BLOBSUMS"
}

# Start NixOS in container, setup firewall, etc. Requires: unshare, passt, bubblewarp.
function chroot_to() {
  SYSROOT="$1"; shift
  unshare --map-auto --map-root-user \
    pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
          --tcp-ports auto --netns-only \
      bwrap --die-with-parent --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
            --bind "$SYSROOT" / --dev /dev --proc /proc --tmpfs /tmp \
            --ro-bind /etc/resolv.conf /etc/resolv.conf \
            --clearenv \
        -- /nix/var/nix/profiles/default/bin/bash \
           --init-file /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh -i "$@"
}

function install_nixos() {
  SYSROOT="$1"
  mkdir -p "$SYSROOT/etc/nixos"
  cp {flake,configuration}.nix "$SYSROOT/etc/nixos/"
  chroot_to "$SYSROOT" -c \
    "nix --extra-experimental-features nix-command\ flakes \
       build /etc/nixos#nixosConfigurations.agenthouse.config.system.build.toplevel \
       --out-link /nix/var/nix/profiles/system"
}

function chroot2() {
  SYSROOT="$1"; shift
  unshare --map-auto --map-root-user \
    pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
          --tcp-ports auto --netns-only \
      bwrap --die-with-parent --unshare-pid --unshare-ipc --unshare-uts\
            --bind "$SYSROOT" / --dev /dev --proc /proc --tmpfs /tmp \
            --ro-bind /sys /sys --bind /sys/fs/cgroup /sys/fs/cgroup \
            --ro-bind /etc/resolv.conf /etc/resolv.conf \
            --clearenv \
        --as-pid-1 /nix/var/nix/profiles/system/init
}
function chroot3() {
  SYSROOT="$1"; shift
  unshare --user --map-auto --map-root-user --ipc --mount --pid --cgroup --time \
    pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
          --tcp-ports auto --netns-only \
      bwrap --die-with-parent --unshare-pid --unshare-ipc --unshare-uts\
            --bind "$SYSROOT" / --dev /dev --proc /proc --tmpfs /tmp \
            --ro-bind /sys /sys --bind /sys/fs/cgroup /sys/fs/cgroup \
            --ro-bind /etc/resolv.conf /etc/resolv.conf \
            --clearenv \
        --as-pid-1 /nix/var/nix/profiles/system/init
}

# fetch_nixos_dockerhub sysroot
#chroot_to sysroot
#install_nixos sysroot
chroot2 sysroot
