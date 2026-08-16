# SPDX-License-Identifier: MIT
"""Installed-browser discovery and explicit URL launching for the desktop UI."""
from __future__ import annotations

import os
import platform
import re
import shutil
import subprocess
import webbrowser
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


@dataclass(frozen=True)
class BrowserChoice:
    """One browser that the launcher can address explicitly."""

    key: str
    label: str
    command: tuple[str, ...] | None


_DEFAULT_BROWSER = BrowserChoice("default", "默认浏览器", None)
_MACOS_BROWSERS = (
    ("chrome", "Google Chrome", "Google Chrome.app"),
    ("edge", "Microsoft Edge", "Microsoft Edge.app"),
    ("safari", "Safari", "Safari.app"),
    ("firefox", "Firefox", "Firefox.app"),
    ("arc", "Arc", "Arc.app"),
    ("brave", "Brave", "Brave Browser.app"),
    ("chromium", "Chromium", "Chromium.app"),
    ("opera", "Opera", "Opera.app"),
    ("vivaldi", "Vivaldi", "Vivaldi.app"),
)
_LINUX_BROWSERS = (
    ("chrome", "Google Chrome", "google-chrome"),
    ("edge", "Microsoft Edge", "microsoft-edge"),
    ("firefox", "Firefox", "firefox"),
    ("brave", "Brave", "brave-browser"),
    ("chromium", "Chromium", "chromium"),
    ("chromium-browser", "Chromium", "chromium-browser"),
    ("opera", "Opera", "opera"),
    ("vivaldi", "Vivaldi", "vivaldi"),
)


def _deduplicate(choices: Iterable[BrowserChoice]) -> tuple[BrowserChoice, ...]:
    seen: set[str] = set()
    result: list[BrowserChoice] = []
    for choice in choices:
        command = choice.command
        identity = choice.key if command is None else os.path.normcase(os.path.abspath(command[-1]))
        if identity in seen:
            continue
        seen.add(identity)
        result.append(choice)
    return tuple(result)


def _discover_macos(roots: Iterable[Path]) -> tuple[BrowserChoice, ...]:
    choices: list[BrowserChoice] = []
    for key, label, app_name in _MACOS_BROWSERS:
        for root in roots:
            application = root / app_name
            if application.is_dir():
                choices.append(
                    BrowserChoice(key, label, ("/usr/bin/open", "-a", str(application))),
                )
                break
    return _deduplicate(choices)


def _windows_command_path(command: str) -> Path | None:
    expanded = os.path.expandvars(command.strip())
    if not expanded:
        return None
    match = re.match(r'^"([^"]+\.exe)"|^([^\s]+\.exe)', expanded, re.IGNORECASE)
    if match is None:
        return None
    return Path(match.group(1) or match.group(2))


def _discover_windows() -> tuple[BrowserChoice, ...]:
    try:
        import winreg
    except ImportError:
        return ()

    choices: list[BrowserChoice] = []
    registry_path = r"Software\Clients\StartMenuInternet"
    for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
        try:
            root = winreg.OpenKey(hive, registry_path)
        except OSError:
            continue
        with root:
            index = 0
            while True:
                try:
                    key_name = winreg.EnumKey(root, index)
                except OSError:
                    break
                index += 1
                try:
                    with winreg.OpenKey(root, key_name) as browser_key:
                        label = str(winreg.QueryValue(browser_key, None) or key_name)
                    with winreg.OpenKey(root, key_name + r"\shell\open\command") as command_key:
                        command = str(winreg.QueryValue(command_key, None) or "")
                except OSError:
                    continue
                executable = _windows_command_path(command)
                if executable is None or not executable.is_file():
                    continue
                choices.append(
                    BrowserChoice(key_name.casefold(), label, (str(executable),)),
                )
    return _deduplicate(choices)


def _discover_linux() -> tuple[BrowserChoice, ...]:
    choices: list[BrowserChoice] = []
    for key, label, executable_name in _LINUX_BROWSERS:
        executable = shutil.which(executable_name)
        if executable is not None:
            choices.append(BrowserChoice(key, label, (executable,)))
    return _deduplicate(choices)


def discover_browsers(system_name: str | None = None) -> tuple[BrowserChoice, ...]:
    """Return installed browsers, or one system-default fallback when unknown."""
    system = system_name or platform.system()
    if system == "Darwin":
        installed = _discover_macos(
            (Path("/Applications"), Path("/System/Applications"), Path.home() / "Applications"),
        )
    elif system == "Windows":
        installed = _discover_windows()
    elif system == "Linux":
        installed = _discover_linux()
    else:
        installed = ()
    return installed or (_DEFAULT_BROWSER,)


def open_in_browser(browser: BrowserChoice, url: str) -> bool:
    """Open a URL with one discovered browser or the system default fallback."""
    if browser.command is None:
        return bool(webbrowser.open(url))
    try:
        subprocess.Popen(
            [*browser.command, url],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            close_fds=True,
        )
    except OSError:
        return False
    return True
