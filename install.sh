#!/usr/bin/env bash
# Build and install dracoshell to $PREFIX (default ~/.local).
# Installs:
#   - $PREFIX/bin/dracoshell           binary
#   - $XDG_DATA_HOME/applications/     .desktop file (so the WM finds the icon)
#   - $XDG_DATA_HOME/icons/hicolor/.../ icon at a few standard sizes
set -euo pipefail

PREFIX="${DRACOSHELL_PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
APPS_DIR="$DATA_DIR/applications"
ICON_BASE="$DATA_DIR/icons/hicolor"

cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found. install Rust from https://rustup.rs" >&2
    exit 1
fi

echo "==> building dracoshell (release)"
cargo build --release --locked

echo "==> installing binary to $BIN_DIR/dracoshell"
mkdir -p "$BIN_DIR"
install -m 0755 target/release/dracoshell "$BIN_DIR/dracoshell"

echo "==> installing desktop entry to $APPS_DIR/dracoshell.desktop"
mkdir -p "$APPS_DIR"
install -m 0644 assets/dracoshell.desktop "$APPS_DIR/dracoshell.desktop"

# Drop the icon at the largest hicolor bucket; the compositor scales as needed.
ICON_DIR="$ICON_BASE/256x256/apps"
echo "==> installing icon to $ICON_DIR/dracoshell.png"
mkdir -p "$ICON_DIR"
install -m 0644 assets/dracoshell.png "$ICON_DIR/dracoshell.png"

# Refresh caches if the helpers are present (best-effort; not all distros ship them).
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APPS_DIR" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$ICON_BASE" 2>/dev/null || true
fi

if ! command -v dracoshell >/dev/null 2>&1; then
    echo
    echo "warning: $BIN_DIR is not in your PATH. add to your shell profile:"
    echo "    export PATH=\"$BIN_DIR:\$PATH\""
fi

echo
echo "done. run \`dracoshell --setup\` to generate a default config."
