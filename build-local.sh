#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Build the macOS desktop launcher (.app + DMG) from a local checkout.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT_DIR"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "build-local.sh only packages macOS. Use build-local.bat on Windows."
  exit 1
fi

case "$(uname -m)" in
  arm64)
    ARCH_SUFFIX="arm64"
    EXPECTED_ARCH="arm64"
    ;;
  x86_64)
    ARCH_SUFFIX="x86_64"
    EXPECTED_ARCH="x86_64"
    ;;
  *) echo "Unsupported macOS architecture: $(uname -m)"; exit 1 ;;
esac

BUILD_PYTHON="${DSH_BUILD_PYTHON:-}"
if [[ -z "$BUILD_PYTHON" ]] && command -v python3.11 >/dev/null 2>&1 \
  && python3.11 -c "import tkinter" >/dev/null 2>&1; then
  BUILD_PYTHON="$(command -v python3.11)"
elif [[ -z "$BUILD_PYTHON" ]] && /usr/bin/python3 -c "import tkinter" >/dev/null 2>&1; then
  BUILD_PYTHON="/usr/bin/python3"
fi
if [[ -z "$BUILD_PYTHON" ]]; then
  echo "A Python installation with Tk is required for packaging."
  echo "Install it with: brew install python-tk@3.11"
  exit 1
fi

BUILD_VENV="$ROOT_DIR/.build-venv"
export PIP_CACHE_DIR="$ROOT_DIR/build-work/pip-cache"
export PYINSTALLER_CONFIG_DIR="$ROOT_DIR/build-work/pyinstaller-config"
mkdir -p "$PIP_CACHE_DIR" "$PYINSTALLER_CONFIG_DIR"
if [[ ! -x "$BUILD_VENV/bin/python" ]]; then
  echo "[1/4] Creating packaging environment..."
  "$BUILD_PYTHON" -m venv "$BUILD_VENV"
fi

echo "[2/4] Preparing PyInstaller..."
"$BUILD_VENV/bin/python" -m pip install --upgrade pip wheel setuptools
"$BUILD_VENV/bin/python" -m pip install -r requirements-runtime.txt
"$BUILD_VENV/bin/python" -m pip install -r requirements-build.txt

echo "[3/4] Building the macOS app..."
"$BUILD_VENV/bin/python" -m PyInstaller --clean --noconfirm \
  --distpath dist --workpath build-work build/mac.spec

APP_BUNDLE="$ROOT_DIR/dist/DSHLauncher.app"
ACTUAL_ARCHS="$(/usr/bin/lipo -archs "$APP_BUNDLE/Contents/MacOS/DSHLauncher")"
if [[ " $ACTUAL_ARCHS " != *" $EXPECTED_ARCH "* ]]; then
  echo "Package architecture mismatch: expected $EXPECTED_ARCH, got $ACTUAL_ARCHS"
  exit 1
fi
/usr/bin/codesign --force --deep --sign - "$APP_BUNDLE"

echo "[4/4] Creating DMG..."
DMG_PATH="$ROOT_DIR/dist/DSHLauncher-macOS-$ARCH_SUFFIX.dmg"
bash "$ROOT_DIR/build/create_macos_dmg.sh" "$APP_BUNDLE" "$DMG_PATH" "DSH Launcher"

echo
echo "Build complete:"
echo "  Installer: $DMG_PATH"
echo
echo "Install by opening the DMG and dragging DSHLauncher.app onto Applications."
