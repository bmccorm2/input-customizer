#!/bin/sh
# Build, install to /Applications, and relaunch.
# Signed with the stable "Keyboard Customizer Dev" cert, so the Accessibility grant survives rebuilds.
set -eu
cd "$(dirname "$0")/.."

APP="Keyboard Customizer.app"
DEST="/Applications/$APP"

security find-identity -p codesigning | grep -q '"Keyboard Customizer Dev"' || {
  echo 'Missing code signing cert "Keyboard Customizer Dev" in the login keychain.' >&2
  exit 1
}

bun run tauri build

# Quit any running release copy (not the `tauri dev` binary).
pkill -f "$APP/Contents/MacOS/keyboard-customizer" || true
sleep 1

rm -rf "$DEST"
ditto "src-tauri/target/release/bundle/macos/$APP" "$DEST"
codesign --verify --strict "$DEST"
open "$DEST"
echo "Installed $DEST"
