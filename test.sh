#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
exec nix build --extra-experimental-features 'nix-command flakes' --file ./test.nix "$@"
