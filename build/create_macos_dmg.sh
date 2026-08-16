#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Create a distributable DMG from an .app bundle.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "Usage: $0 <app-path> <dmg-path> <volume-name>" >&2
  exit 2
fi

APP_PATH="$1"
DMG_PATH="$2"
VOLUME_NAME="$3"
HDIUTIL="${HDIUTIL:-/usr/bin/hdiutil}"

if [[ ! -d "$APP_PATH" ]]; then
  echo "Application bundle not found: $APP_PATH" >&2
  exit 2
fi

STAGING_DIR="$(mktemp -d "${TMPDIR:-/tmp}/dsh-desktop-dmg.XXXXXX")"
trap 'rm -rf "$STAGING_DIR"' EXIT

cp -R "$APP_PATH" "$STAGING_DIR/"
ln -s /Applications "$STAGING_DIR/Applications"

rm -f "$DMG_PATH"
"$HDIUTIL" create \
  -volname "$VOLUME_NAME" \
  -srcfolder "$STAGING_DIR" \
  -ov \
  -format UDZO \
  "$DMG_PATH"
"$HDIUTIL" verify "$DMG_PATH" >/dev/null
