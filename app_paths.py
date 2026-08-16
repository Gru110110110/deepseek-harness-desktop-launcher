# SPDX-License-Identifier: MIT
"""Application paths and constants for DSH Launcher.

The launcher is a thin shell: on first launch it downloads a Node.js runtime and
``npm install``-s the published ``@deepseek-ai/dsh`` package into a per-user home
directory, starts ``dsh web`` as a child process, and shows a small status
window. This module owns the names, versions, and on-disk layout shared by the
other modules.
"""
from __future__ import annotations

import os
import platform
import sys
from dataclasses import dataclass
from pathlib import Path

APP_NAME = "DSH Launcher"
APP_ID = "dsh-desktop"
# The desktop shell's own version.
APP_VERSION = "0.1.0"

# The desktop release pins one exact Node build whose per-platform hashes live in
# runtime.py. A version override must supply DSH_DESKTOP_NODE_SHA256 as its trust
# root; changing transport mirrors never changes the expected bytes.
NODE_VERSION = "24.19.0"
NODE_VERSION_MAJOR = NODE_VERSION.split(".", maxsplit=1)[0]
NODE_DIST_BASE_DEFAULT = "https://nodejs.org/dist"
NODE_DIST_BASE_FALLBACK = "https://npmmirror.com/mirrors/node"


def node_dist_base() -> str:
    """The first configured Node.js distribution base URL."""
    return os.environ.get("DSH_DESKTOP_NODE_BASE", NODE_DIST_BASE_DEFAULT)


def node_dist_bases() -> tuple[str, ...]:
    """Ordered Node.js distribution bases with explicit configuration respected."""
    configured = os.environ.get("DSH_DESKTOP_NODE_BASES")
    if configured:
        values = tuple(value.strip().rstrip("/") for value in configured.split(",") if value.strip())
        if values:
            return values
    configured = os.environ.get("DSH_DESKTOP_NODE_BASE")
    if configured:
        return (configured.rstrip("/"),)
    return (NODE_DIST_BASE_DEFAULT, NODE_DIST_BASE_FALLBACK)


@dataclass(frozen=True)
class ApplicationPaths:
    """Resolved on-disk layout for one user's launcher installation."""

    app_home: Path
    runtime_dir: Path
    cache_dir: Path
    node_dir: Path
    node_bin: Path
    dsh_dir: Path
    dsh_bin: Path
    version_file: Path
    server_log: Path
    install_log: Path
    server_pid: Path
    language_file: Path
    dsh_home: Path
    home_import_marker: Path
    workspace_import_marker: Path
    cc_switch_import_marker: Path
    deployment_lock: Path

    @classmethod
    def from_home(cls, app_home: Path) -> "ApplicationPaths":
        home = app_home.expanduser()
        runtime_dir = home / "runtime"
        node_dir = runtime_dir / "node"
        return cls(
            app_home=home,
            runtime_dir=runtime_dir,
            cache_dir=home / "cache",
            node_dir=node_dir,
            node_bin=_node_binary(node_dir),
            dsh_dir=runtime_dir / "dsh",
            dsh_bin=runtime_dir / "dsh" / "node_modules" / "@deepseek-ai" / "dsh" / "lib" / "bin.js",
            version_file=runtime_dir / "runtime.version",
            server_log=home / "server.log",
            install_log=home / "install.log",
            server_pid=home / "server.pid",
            language_file=home / "language",
            dsh_home=home / "dsh-home",
            home_import_marker=home / ".source-home-import-v1",
            workspace_import_marker=home / ".source-workspace-import-v1",
            cc_switch_import_marker=home / ".cc-switch-import-v2",
            deployment_lock=runtime_dir / ".deployment.lock",
        )

    @classmethod
    def from_environment(cls) -> "ApplicationPaths":
        configured_home = os.getenv("DSH_DESKTOP_HOME")
        return cls.from_home(
            Path(configured_home) if configured_home else Path.home() / ".dsh-desktop",
        )

    def ensure_dirs(self) -> None:
        """Create every directory the launcher writes into."""
        self.app_home.mkdir(parents=True, exist_ok=True)
        self.runtime_dir.mkdir(parents=True, exist_ok=True)
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        self.dsh_home.mkdir(parents=True, exist_ok=True)


def _node_binary(node_dir: Path) -> Path:
    if os.name == "nt":
        return node_dir / "node.exe"
    return node_dir / "bin" / "node"


def os_tag() -> str:
    """The OS token used in asset names: ``macos``, ``windows``, or ``linux``."""
    system = platform.system()
    if system == "Darwin":
        return "macos"
    if system == "Windows":
        return "windows"
    return system.lower()


def node_platform() -> str:
    """The Node.js distribution platform token: ``darwin``, ``win``, or ``linux``."""
    system = platform.system()
    if system == "Darwin":
        return "darwin"
    if system == "Windows":
        return "win"
    return system.lower()


def arch_tag() -> str:
    """The architecture token: ``arm64`` or ``x64``."""
    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        return "arm64"
    if machine in ("x86_64", "amd64"):
        return "x64"
    return machine


def frozen() -> bool:
    """Whether this process runs inside a PyInstaller bundle."""
    return bool(getattr(sys, "frozen", False))


def resource_root() -> Path:
    """Root directory for bundled assets (icons), in source and frozen layouts."""
    frozen_root = getattr(sys, "_MEIPASS", None)
    return Path(frozen_root) if frozen_root else Path(__file__).resolve().parent


APPLICATION_PATHS = ApplicationPaths.from_environment()
APP_HOME = APPLICATION_PATHS.app_home
RUNTIME_DIR = APPLICATION_PATHS.runtime_dir
NODE_DIR = APPLICATION_PATHS.node_dir
NODE_BIN = APPLICATION_PATHS.node_bin
DSH_BIN = APPLICATION_PATHS.dsh_bin
VERSION_FILE = APPLICATION_PATHS.version_file
SERVER_LOG = APPLICATION_PATHS.server_log
SERVER_PID = APPLICATION_PATHS.server_pid
