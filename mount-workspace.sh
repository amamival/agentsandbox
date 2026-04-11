#!/usr/bin/env bash
set -euo pipefail

die() { echo "$0: $*" >&2; exit 1; }
map_inner_1000() {
  awk '1000 >= $1 && 1000 < ($1 + $3) { print $2 + 1000 - $1; found = 1; exit } END { exit !found }' "$1" ||
    die "inner $2 1000 is not mapped in $1"
}
dst_for() { printf '%s%s\n' "$ws" "$1"; }
mount_one() {
  local src="$1" dst cur
  dst="$(dst_for "$src")"
  if mountpoint -q "$dst"; then
    cur="$(findmnt -n -o SOURCE --target "$dst" || true)"
    [[ "$cur" == "$src" ]] && { echo "/workspace$src is already mounted"; return; }
    die "$dst is already mounted from $cur"
  fi
  sudo sh -eu -c 'install -d "$1" && mount --bind --mkdir --map-users "$2:$3:1" --map-groups "$4:$5:1" "$6" "$7"' sh \
    "$(dirname "$dst")" "$uid" "$UID" "$gid" "$(id -g)" "$src" "$dst"
  echo "$src -> /workspace$src"
}
unmount_one() {
  local src="$1" dst cur
  dst="$(dst_for "$src")"
  mountpoint -q "$dst" || return
  cur="$(findmnt -n -o SOURCE --target "$dst" || true)"
  [[ -z "$cur" || "$cur" == "$src" ]] || die "$dst is mounted from $cur"
  sudo umount "$dst"
  echo "/workspace$src unmounted"
}

main() {
  local app=/sandbox ws=/sandbox/persistent/workspace mounts=/sandbox/mounts pid cmd="${1:-}" src uid gid tmp
  local -a lines=()

  case "$cmd" in
    add|delete|mount|unmount) shift ;;
    -h|--help|'') echo "usage: $0 {add|delete|mount|unmount} [path ...]"; exit 0 ;;
    *) die "unknown subcommand: $cmd" ;;
  esac

  case "$cmd" in
    add|mount)
      [[ -r "$app/sysroot.pid" ]] || die "cannot read $app/sysroot.pid"
      pid="$(pgrep --ns "$(<"$app/sysroot.pid")" --nslist user -f /run/current-system/systemd/lib/systemd/systemd || true)"
      [[ -n "$pid" && -r "/proc/$pid/uid_map" && -r "/proc/$pid/gid_map" ]] || die "cannot find running sandbox idmap"
      uid="$(map_inner_1000 "/proc/$pid/uid_map" UID)"
      gid="$(map_inner_1000 "/proc/$pid/gid_map" GID)"
      ;;
  esac

  case "$cmd" in
    add)
      [[ "$#" -gt 0 ]] || die "add requires at least one path"
      sudo install -D -m 644 /dev/null "$mounts"
      for src in "$@"; do
        src="$(realpath -e "$src")"
        [[ -d "$src" ]] || die "only directories are supported: $src"
        grep -Fxq -- "$src" "$mounts" || printf '%s\n' "$src" | sudo tee -a "$mounts" >/dev/null
        mount_one "$src"
      done
      ;;
    delete)
      [[ "$#" -gt 0 ]] || die "delete requires at least one path"
      sudo install -D -m 644 /dev/null "$mounts"
      for src in "$@"; do
        src="$(realpath -m "$src")"
        tmp="$(mktemp)"
        grep -Fxv -- "$src" "$mounts" >"$tmp" || true
        sudo install -m 644 "$tmp" "$mounts"
        rm -f "$tmp"
        unmount_one "$src"
      done
      ;;
    mount)
      [[ -r "$mounts" ]] || return
      while IFS= read -r src; do [[ -n "$src" ]] && mount_one "$src"; done < "$mounts"
      ;;
    unmount)
      [[ -r "$mounts" ]] || return
      mapfile -t lines < "$mounts"
      for ((i=${#lines[@]}-1; i>=0; i--)); do [[ -n "${lines[i]}" ]] && unmount_one "${lines[i]}"; done
      ;;
  esac
}

main "$@"
