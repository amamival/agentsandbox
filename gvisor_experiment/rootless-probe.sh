#!/usr/bin/env bash
set -euo pipefail

SELF="$(basename "$0")"
STATE_ROOT="${HOME}/.local/state/agenthouse-gvisor-rootless"
RUNSC_ROOT="${STATE_ROOT}/runsc"
NETWORK_URL="http://example.com/"
ACTION="all"

function die() {
  echo "${SELF}: $*" >&2
  exit 1
}

function note() {
  echo "==> $*"
}

function usage() {
  cat <<'EOF'
usage: ./gvisor/rootless-probe.sh [all|do|run|overlay|create-limit|cleanup]
                                  [--state-root PATH] [--network-url URL]

This is a rootless gVisor probe for environments where sudo is blocked.
It proves the subset that actually works here:
  - `runsc --rootless do`
  - `runsc --rootless run`
  - host networking in rootless mode
  - tmpfs overlay writes not leaking back to the host

It also records the known limit we care about:
  - `runsc --rootless create` is unsupported
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    all|do|run|overlay|create-limit|cleanup)
      ACTION="$1"
      shift
      ;;
    --state-root)
      STATE_ROOT="$2"
      shift 2
      ;;
    --network-url)
      NETWORK_URL="$2"
      shift 2
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

RUNSC_ROOT="${STATE_ROOT}/runsc"

function require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

function preflight() {
  require_command runsc
  require_command jq
  require_command curl
  require_command mktemp
  require_command realpath
  install -d -m 0755 "$STATE_ROOT" "$RUNSC_ROOT"
}

function rootless_runsc() {
  runsc --rootless --root "$RUNSC_ROOT" --network=host "$@"
}

function rootless_do() {
  local out=''
  note "probing runsc --rootless do"
  out="$(
    rootless_runsc "do" -- /bin/sh -c \
      "id; pwd; '$(command -v curl)' --connect-timeout 5 --max-time 10 -fsSL '$NETWORK_URL' >/dev/null"
  )"
  printf '%s\n' "$out"
  grep -Fq 'uid=0(root)' <<<"$out" || die "rootless do did not report uid=0"
}

function write_run_bundle() {
  local bundle="$1" tmp=''
  rm -rf "$bundle"
  install -d -m 0755 "$bundle"
  (
    cd "$bundle"
    runsc spec -- /bin/sh -c \
      "'$(command -v id)'"
  )
  tmp="$(mktemp)"
  jq '
    .root.path = "/" |
    .root.readonly = true |
    .process.cwd = "/" |
    .process.terminal = false |
    .mounts |= map(
      select(.destination != "/run" and .destination != "/tmp" and .destination != "/etc/resolv.conf")
    ) + [
      {
        "destination": "/etc/resolv.conf",
        "type": "bind",
        "source": "/etc/resolv.conf",
        "options": ["rbind", "ro"]
      },
      {
        "destination": "/run",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=755", "size=16m"]
      },
      {
        "destination": "/tmp",
        "type": "tmpfs",
        "source": "tmpfs",
        "options": ["nosuid", "nodev", "mode=1777", "size=64m"]
      }
    ]
  ' "$bundle/config.json" >"$tmp"
  mv "$tmp" "$bundle/config.json"
}

function rootless_run_probe() {
  local bundle='' out=''
  bundle="$(mktemp -d)"
  trap 'rm -rf "$bundle"' RETURN
  write_run_bundle "$bundle"
  note "probing runsc --rootless run"
  out="$(rootless_runsc run --bundle "$bundle" rootless-run-probe)"
  printf '%s\n' "$out"
  grep -Fq 'uid=0(root)' <<<"$out" || die "rootless run did not report uid=0"
}

function overlay_probe() {
  local host_dir='' out=''
  host_dir="$(mktemp -d)"
  trap 'rm -rf "$host_dir"' RETURN
  printf '%s\n' host-visible >"$host_dir/marker.txt"
  note "probing rootless overlay isolation"
  out="$(
    rootless_runsc "do" -cwd "$host_dir" -- /bin/sh -c \
      "test -f marker.txt; printf '%s\n' sandbox-only > created.txt; test -f created.txt; pwd"
  )"
  printf '%s\n' "$out"
  [[ -f "$host_dir/marker.txt" ]] || die "host marker disappeared"
  [[ ! -e "$host_dir/created.txt" ]] || die "overlay write leaked back to host"
}

function create_limit_probe() {
  local bundle='' out='' status=0
  bundle="$(mktemp -d)"
  trap 'rm -rf "$bundle"' RETURN
  write_run_bundle "$bundle"
  note "probing known rootless create limitation"
  set +e
  out="$(rootless_runsc create --bundle "$bundle" rootless-create-probe 2>&1)"
  status=$?
  set -e
  printf '%s\n' "$out"
  [[ $status -ne 0 ]] || die "rootless create unexpectedly succeeded"
  grep -Fq 'Rootless mode not supported with "create"' <<<"$out" ||
    die "rootless create failed, but not for the expected reason"
}

function cleanup() {
  rm -rf "$RUNSC_ROOT"
}

preflight

case "$ACTION" in
  do)
    rootless_do
    ;;
  run)
    rootless_run_probe
    ;;
  overlay)
    overlay_probe
    ;;
  create-limit)
    create_limit_probe
    ;;
  cleanup)
    cleanup
    ;;
  all)
    rootless_do
    rootless_run_probe
    overlay_probe
    create_limit_probe
    note "rootless probe passed"
    ;;
  *)
    die "unknown action: $ACTION"
    ;;
esac
