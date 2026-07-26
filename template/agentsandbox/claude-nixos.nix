# Adapted from https://github.com/ducks/claude-nixos.
{ pkgs }:

pkgs.writeShellScriptBin "update_claude_fixed" ''
  export PATH=${pkgs.lib.makeBinPath (with pkgs; [ bash coreutils curl file findutils gnugrep gnused patchelf ])}:$PATH
  export CLAUDE_NIXOS_LINKER="${pkgs.stdenv.cc.bintools.dynamicLinker}"

  patch_binary() {
    [ -n "$1" ] && [ -f "$1" ] || { echo "patch_binary: file not found: $1" >&2; return 1; }
    file -b "$1" | grep -q "ELF.*executable" || return 0
    chmod u+w "$1" 2>/dev/null; patchelf --set-interpreter "$CLAUDE_NIXOS_LINKER" "$1"
  }
  export -f patch_binary

  install_claude_wrapper() {
    mkdir -p "$HOME/.local/bin"
    rm -f "$HOME/.local/bin/claude"
    cat > "$HOME/.local/bin/claude" <<${"'"}W${"'"}
#!/usr/bin/env bash
set -e
d="''$HOME/.local/share/claude/versions"
b="$d/$(ls -1 "$d" 2>/dev/null | grep -E "^[0-9]+\.[0-9]+\.[0-9]+" | sort -V | tail -1)"
[ -f "$b" ] || { echo "claude wrapper: no Claude binary found under $d" >&2; exit 1; }
exec -a "''${BASH_ARGV0:-$0}" "$b" "$@"
W
    chmod +x "$HOME/.local/bin/claude"
  }

  patch_installed_claude() {
    local d p
    for d in "$HOME/.local/share/claude/versions" "$HOME/.claude/downloads"; do
      [ -d "$d" ] || continue
      while IFS= read -r -d "" p; do patch_binary "$p"; done < <(find "$d" -maxdepth 1 -type f -perm -0100 -print0)
    done
  }

  t=$(mktemp -d); trap "rm -rf \"$t\"" RETURN
  curl -fsSL https://claude.ai/install.sh -o "$t/install.sh"
  chmod +x "$t/install.sh"
  sed -i "/chmod +x \"\$binary_path\"/a patch_binary \"\$binary_path\"" "$t/install.sh"
  bash "$t/install.sh" latest
  patch_installed_claude
  install_claude_wrapper
''
