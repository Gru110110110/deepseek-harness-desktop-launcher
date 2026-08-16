# -*- mode: python ; coding: utf-8 -*-
# SPDX-License-Identifier: MIT
# PyInstaller spec: one-folder .app bundle for the macOS desktop launcher.
import sys
from importlib.metadata import distribution
from pathlib import Path

root = Path(SPECPATH).resolve().parent
sys.path.insert(0, str(root))
from app_paths import APP_NAME, APP_VERSION

icon = root / "assets" / "icon.icns"
pystray_dist = distribution("pystray")
pystray_copying = next(
    entry for entry in pystray_dist.files
    if str(entry).endswith(".dist-info/COPYING")
)
pystray_license = Path(pystray_dist.locate_file(pystray_copying))

a = Analysis(
    [str(root / "main.py")],
    pathex=[str(root)],
    binaries=[],
    datas=[
        (str(root / "assets"), "assets"),
        (str(root / "THIRD_PARTY_NOTICES.md"), "."),
        (str(pystray_license), "licenses/pystray"),
    ],
    hiddenimports=[
        "tkinter",
        "tkinter.ttk",
        "pystray",
        "pystray._darwin",
        "PIL.Image",
        "AppKit",
        "Quartz",
    ],
    hookspath=[],
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
)
pyz = PYZ(a.pure)
exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="DSHLauncher",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=False,
    target_arch=None,
)
coll = COLLECT(exe, a.binaries, a.datas, strip=False, upx=False, name="DSHLauncher")
app = BUNDLE(
    coll,
    name="DSHLauncher.app",
    icon=str(icon),
    bundle_identifier="com.gru.dsh-launcher",
    info_plist={
        "CFBundleDisplayName": APP_NAME,
        "CFBundleName": APP_NAME,
        "CFBundleShortVersionString": APP_VERSION,
        "NSHighResolutionCapable": True,
    },
)
