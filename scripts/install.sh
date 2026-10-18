#!/usr/bin/env bash
# Builds aq in release mode and installs the binary for the current user:
#
#   bin/aq    the aq CLI and TUI
#
# All paths are below $PREFIX (default: ~/.local).
#
# Usage: scripts/install.sh [--no-build] [--uninstall]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"

build=1
uninstall=0
for arg in "$@"; do
  case "$arg" in
    --no-build) build=0 ;;
    --uninstall) uninstall=1 ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
    *) echo "error: unknown option $arg" >&2; exit 1 ;;
  esac
done

if [[ $uninstall == 1 ]]; then
  rm -f "$BIN_DIR/aq"
  echo "aq is uninstalled from $PREFIX."
  exit 0
fi

if [[ $build == 1 ]]; then
  (cd "$ROOT" && cargo build --release --locked)
fi

BINARY="$ROOT/target/release/aq"
if [[ ! -x "$BINARY" ]]; then
  echo "error: $BINARY does not exist. Run without --no-build." >&2
  exit 1
fi

install -Dm755 "$BINARY" "$BIN_DIR/aq"
echo "Installed: $BIN_DIR/aq"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Note: $BIN_DIR is not on your PATH." ;;
esac
