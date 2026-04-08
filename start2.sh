#!/usr/bin/env bash
# Start NixOS in container, setup firewall, etc. Require passt, bubblewarp.
#exec {info_fd}<> >(./init-fw.sh)

pasta --foreground --config-net --map-host-loopback 10.0.2.2 \
      --tcp-ports 2222:22 \
  -- sh -c 'exec "$@"' _ \
    bwrap --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup \
          --bind sysroot / --dev /dev --proc /proc --tmpfs /tmp \
          --ro-bind /etc/resolv.conf /etc/resolv.conf \
          --clearenv \
      -- /bin/sh &
PID=$!
echo PID=$PID
newuidmap $PID 0 100000 6553
          # --uid $UID --gid $(id -g) \
# --ro-bind? hosts ssl passwd group nsswitch.conf locale.gen localtime
