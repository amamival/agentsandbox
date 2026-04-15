#!/usr/bin/env bash
set -euo pipefail

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT
scenario="${1:-}"
script="${AGSB_TEST_SCRIPT:-}"

if [[ -z "$script" && -f ./agentsandbox ]]; then
  script="$(readlink -f ./agentsandbox)"
fi
[[ -n "$script" && -f "$script" ]] || {
  printf 'tests.sh: AGSB_TEST_SCRIPT must point to agentsandbox\n' >&2
  exit 1
}

export HOME="$workdir/home"
export XDG_CONFIG_HOME="$workdir/config"
export XDG_DATA_HOME="$workdir/data"
export XDG_STATE_HOME="$workdir/state"
export XDG_RUNTIME_DIR="$workdir/runtime"
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR"
project="$workdir/project"
mkdir -p "$project"
cd "$project"

case "$scenario" in
  design-contract)
    bash "$script" init
    bash "$script" help | grep -F "proxy-logs" >/dev/null
    bash "$script" version | grep -E '^[^[:space:]]+$' >/dev/null
    bash "$script" doctor | grep -F "uri:" >/dev/null
    eval "$(bash "$script" __resolve-active-config "$PWD")"
    [[ "$ACTIVE_CONFIG_DIR" == "$PWD/.agentsandbox" ]]
    eval "$(bash "$script" __resolve-instance "$PWD" "$PWD/.agentsandbox" default)"
    [[ "$INSTANCE_ID" == "${INSTANCE_NAME}-${MACHINE_ID}" ]]
    bash "$script" allow-domain Example.COM
    bash "$script" allow-domain https://example.com/path
    bash "$script" allow-domain 'https://*.Example.COM.:8443/path'
    [[ "$(grep -Fxc "example.com" .agentsandbox/allowed_hosts)" == "1" ]]
    grep -Fx "*.example.com" .agentsandbox/allowed_hosts >/dev/null
    bash "$script" unallow-domain https://EXAMPLE.com.:443/path
    ! grep -Fx "example.com" .agentsandbox/allowed_hosts >/dev/null
    mkdir -p alpha beta
    bash "$script" mount ./alpha
    bash "$script" mount ./beta sandbox-beta
    grep -F "$(realpath alpha)	alpha" .agentsandbox/mounts >/dev/null
    grep -F "$(realpath beta)	sandbox-beta" .agentsandbox/mounts >/dev/null
    bash "$script" unmount ./alpha
    ! grep -F "$(realpath alpha)	alpha" .agentsandbox/mounts >/dev/null
    ;;

  testmd-e2e)
    bash "$script" init
    if [[ -f "$(dirname "$script")/flake.lock" ]]; then
      cp "$(dirname "$script")/flake.lock" .agentsandbox/flake.lock
    fi
    if [[ "${AGSB_TEST_LOCAL_INPUTS:-0}" == "1" ]]; then
      sed -i \
        -e "s|github:NixOS/nixpkgs/nixos-25.11|path:${AGSB_TEST_NIXPKGS_PATH}|" \
        -e "s|github:nix-community/home-manager/release-25.11|path:${AGSB_TEST_HOME_MANAGER_PATH}|" \
        -e "s|github:nix-community/impermanence|path:${AGSB_TEST_IMPERMANENCE_PATH}|" \
        .agentsandbox/flake.nix
    fi
    bash "$script" help >/dev/null
    bash "$script" version >/dev/null
    bash "$script" doctor >/dev/null
    eval "$(bash "$script" __resolve-active-config "$PWD")"
    eval "$(bash "$script" __resolve-instance "$PWD" "$PWD/.agentsandbox" default)"
    bash "$script" build
    [[ -d "$DATA_DIR/sysroot" ]]
    [[ -d "$DATA_DIR/sysroot/nix/store" ]]
    machine_id_first="$(cat "$DATA_DIR/machine-id")"
    bash "$script" build
    machine_id_second="$(cat "$DATA_DIR/machine-id")"
    [[ "$machine_id_first" == "$machine_id_second" ]]
    mkdir -p alpha beta
    bash "$script" mount ./alpha
    bash "$script" mount ./beta sandbox-beta
    bash "$script" allow-domain Example.COM
    bash "$script" allow-domain https://example.com/path
    bash "$script" allow-domain 'https://*.Example.COM.:8443/path'
    bash "$script" unallow-domain https://EXAMPLE.com.:443/path
    bash "$script" up
    bash "$script" ps | grep -E '(running|paused)' >/dev/null
    bash "$script" port >/dev/null
    bash "$script" port 22 tcp | grep -E '^[0-9]+$' >/dev/null
    bash "$script" exec -- uname -a >/dev/null
    bash "$script" logs >/dev/null
    bash "$script" stats | grep -F "CPU %" >/dev/null
    bash "$script" pause
    bash "$script" ps | grep -F "paused" >/dev/null
    bash "$script" unpause
    bash "$script" ps | grep -F "running" >/dev/null
    bash "$script" down || true
    set +e
    timeout 60 bash "$script" wait
    wait_status="$?"
    set -e
    [[ "$wait_status" == "0" || "$wait_status" == "124" ]]
    if bash "$script" ps | grep -F "running" >/dev/null; then
      bash "$script" kill
    fi
    bash "$script" up
    bash "$script" kill
    bash "$script" destroy
    [[ ! -d "$DATA_DIR/sysroot" ]]
    [[ -d "$DATA_DIR/persistent" ]]
    ;;

  *)
    printf 'usage: %s {design-contract|testmd-e2e}\n' "$0" >&2
    exit 1
    ;;
esac
