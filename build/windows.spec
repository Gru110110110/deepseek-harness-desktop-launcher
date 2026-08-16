# -*- mode: python ; coding: utf-8 -*-
# SPDX-License-Identifier: MIT
# PyInstaller spec: one-file executable for the Windows desktop launcher.
import sys
from importlib.metadata import distribution
from pathlib import Path

root = Path(SPECPATH).resolve().parent
sys.path.insert(0, str(root))
from app_paths import APP_NAME

icon = root / "assets" / "icon.ico"
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
        "pystray._win32",
        "PIL.Image",
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
    a.binaries,
    a.datas,
    [],
    name="DSHLauncher",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=False,
    icon=str(icon),
)
