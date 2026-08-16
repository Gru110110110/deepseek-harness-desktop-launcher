# SPDX-License-Identifier: MIT
"""Bounded and transactional deployment of the desktop Node and Harness runtime.

The desktop release pins one exact Node archive. A first Harness installation
selects the highest valid ``latest`` value returned by the configured registries,
then freezes that value as the deployment's exact target. Downloads may use
several transports, but Node bytes are admitted only after SHA-256 verification.
Node and Harness are prepared in sibling staging directories, driven through
bounded subprocesses, and published only after executable smokes pass. A failed
update therefore leaves the previously active runtime available.
"""
from __future__ import annotations

import concurrent.futures
import hashlib
import json
import os
import posixpath
import re
import shutil
import signal
import socket
import stat
import subprocess
import tarfile
import threading
import time
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import uuid
import zipfile
from functools import cmp_to_key
from pathlib import Path, PurePosixPath
from typing import BinaryIO, Callable, TextIO

from app_paths import (
    NODE_DIST_BASE_DEFAULT,
    NODE_DIST_BASE_FALLBACK,
    NODE_VERSION,
    ApplicationPaths,
    arch_tag,
    node_dist_bases,
    node_platform,
)
from localization import LocalizedError

ProgressCallback = Callable[[int, int], None]
StepCallback = Callable[[str], None]
ActivityCallback = Callable[[str, dict[str, object]], None]

_CHUNK_SIZE = 64 * 1024
_NETWORK_TIMEOUT_SECONDS = 10.0
_DOWNLOAD_TIMEOUT_SECONDS = 10 * 60.0
_INSTALL_TIMEOUT_SECONDS = 15 * 60.0
_SMOKE_TIMEOUT_SECONDS = 30.0
_LOCK_TIMEOUT_SECONDS = 15 * 60.0
_PROCESS_GRACE_SECONDS = 3.0
_DOWNLOAD_ATTEMPTS_PER_SOURCE = 2

NPM_REGISTRY_DEFAULT = "https://registry.npmjs.org"
NPM_REGISTRY_FALLBACK = "https://registry.npmmirror.com"

_NODE_ARCHIVE_SHA256 = {
    "node-v24.19.0-darwin-arm64.tar.gz": "8294b7aa9b03997481c06babf1e8b270c859358f27da57a11509afe537ac381d",
    "node-v24.19.0-darwin-x64.tar.gz": "d1b5e999db158c62fe8f7267a4476b035d8bd93b1a605bac24a3f0dd166e3316",
    "node-v24.19.0-win-arm64.zip": "8502f4a50b458d4cc38ed8f2001556c2cd239d464920f74017926ccb1e1c157f",
    "node-v24.19.0-win-x64.zip": "57f71ab3652e797d84acddc79c81cc9ff1c6ddb2a1974cdb83f00fee9bff4c73",
}
_SEMVER = re.compile(
    r"^(?P<major>0|[1-9]\d*)\.(?P<minor>0|[1-9]\d*)\.(?P<patch>0|[1-9]\d*)"
    r"(?:-(?P<prerelease>[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$",
)


class DeploymentCancelled(RuntimeError):
    """The owner cancelled runtime deployment and all owned work has stopped."""


class DeploymentController:
    """Own cancellation and the one subprocess active during a deployment."""

    def __init__(self) -> None:
        self.cancelled = threading.Event()
        self._lock = threading.Lock()
        self._process: subprocess.Popen[bytes] | None = None

    def check(self) -> None:
        """Raise after cancellation so each blocking phase stops at its next bound."""
        if self.cancelled.is_set():
            raise DeploymentCancelled()

    def cancel(self, *, force: bool = False) -> None:
        """Request cancellation and signal the complete active process group."""
        self.cancelled.set()
        with self._lock:
            process = self._process
        if process is not None and process.poll() is None:
            _signal_process_tree(process, force=force)

    def run(
        self,
        command: list[str],
        *,
        cwd: Path,
        stdout: TextIO | int,
        timeout: float,
        environment: dict[str, str],
    ) -> int:
        """Run one owned subprocess to exit, timeout, or complete cancellation."""
        self.check()
        options: dict[str, object]
        if os.name == "nt":
            options = {
                "creationflags": subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.CREATE_NO_WINDOW,
            }
        else:
            options = {"start_new_session": True}
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdout=stdout,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            env=environment,
            **options,
        )
        with self._lock:
            self._process = process
        deadline = time.monotonic() + timeout
        try:
            while True:
                returncode = process.poll()
                if returncode is not None:
                    return returncode
                if self.cancelled.wait(0.1):
                    _stop_process_tree(process)
                    raise DeploymentCancelled()
                if time.monotonic() >= deadline:
                    _stop_process_tree(process)
                    raise subprocess.TimeoutExpired(command, timeout)
        finally:
            with self._lock:
                if self._process is process:
                    self._process = None


def _signal_process_tree(process: subprocess.Popen[bytes], *, force: bool) -> None:
    if process.poll() is not None:
        return
    try:
        if os.name == "nt":
            subprocess.run(
                ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                capture_output=True,
                timeout=5,
                check=False,
                creationflags=subprocess.CREATE_NO_WINDOW,
            )
        else:
            os.killpg(process.pid, signal.SIGKILL if force else signal.SIGTERM)
    except (OSError, subprocess.SubprocessError):
        try:
            process.kill() if force else process.terminate()
        except OSError:
            pass


def _stop_process_tree(process: subprocess.Popen[bytes]) -> None:
    _signal_process_tree(process, force=False)
    try:
        process.wait(timeout=_PROCESS_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        _signal_process_tree(process, force=True)
        try:
            process.wait(timeout=_PROCESS_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            pass


def _environment_seconds(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        value = float(raw)
    except ValueError as exc:
        raise LocalizedError("environment_invalid", variable=name, value=raw) from exc
    if value <= 0:
        raise LocalizedError("environment_invalid", variable=name, value=raw)
    return value


def deployment_shutdown_timeout_seconds() -> float:
    """Maximum close-time wait for an owned network operation to observe cancellation."""
    return _environment_seconds(
        "DSH_DESKTOP_NETWORK_TIMEOUT_SECONDS",
        _NETWORK_TIMEOUT_SECONDS,
    ) + _PROCESS_GRACE_SECONDS + 1


def _display_source(url: str) -> str:
    """Render a transport URL without user information, query data, or fragments."""
    try:
        parts = urllib.parse.urlsplit(url)
        hostname = parts.hostname or ""
        port = parts.port
    except ValueError:
        return "<invalid source>"
    if ":" in hostname and not hostname.startswith("["):
        hostname = f"[{hostname}]"
    if port is not None:
        hostname = f"{hostname}:{port}"
    return urllib.parse.urlunsplit((parts.scheme, hostname, parts.path, "", ""))


def npm_registries(override: str | None = None) -> tuple[str, ...]:
    """Ordered registries; any explicit selection suppresses public fallbacks."""
    if override:
        return (override.rstrip("/"),)
    configured = os.environ.get("DSH_DESKTOP_NPM_REGISTRIES")
    if configured:
        values = tuple(value.strip().rstrip("/") for value in configured.split(",") if value.strip())
        if values:
            return values
    configured = os.environ.get("DSH_DESKTOP_NPM_REGISTRY")
    if configured:
        return (configured.rstrip("/"),)
    return (NPM_REGISTRY_DEFAULT, NPM_REGISTRY_FALLBACK)


def _npm_registry(override: str | None = None) -> str:
    """Compatibility helper returning the first selected registry."""
    return npm_registries(override)[0]


def _parse_semver(value: str) -> tuple[tuple[int, int, int], tuple[str, ...] | None]:
    match = _SEMVER.fullmatch(value)
    if match is None:
        raise ValueError(f"invalid semantic version: {value}")
    core = (int(match.group("major")), int(match.group("minor")), int(match.group("patch")))
    prerelease = match.group("prerelease")
    return core, tuple(prerelease.split(".")) if prerelease is not None else None


def compare_versions(left: str, right: str) -> int:
    """Compare two SemVer values, ignoring build metadata."""
    left_core, left_pre = _parse_semver(left)
    right_core, right_pre = _parse_semver(right)
    if left_core != right_core:
        return (left_core > right_core) - (left_core < right_core)
    if left_pre is None or right_pre is None:
        if left_pre is right_pre:
            return 0
        return 1 if left_pre is None else -1
    for left_item, right_item in zip(left_pre, right_pre):
        if left_item == right_item:
            continue
        left_numeric = left_item.isdigit()
        right_numeric = right_item.isdigit()
        if left_numeric and right_numeric:
            return (int(left_item) > int(right_item)) - (int(left_item) < int(right_item))
        if left_numeric != right_numeric:
            return -1 if left_numeric else 1
        return (left_item > right_item) - (left_item < right_item)
    return (len(left_pre) > len(right_pre)) - (len(left_pre) < len(right_pre))


def is_newer_version(candidate: str, current: str) -> bool:
    """Whether ``candidate`` is strictly newer than ``current`` under SemVer."""
    try:
        return compare_versions(candidate, current) > 0
    except ValueError:
        return False


def _query_registry_version(registry: str, controller: DeploymentController | None) -> str:
    if controller is not None:
        controller.check()
    url = f"{registry}/@deepseek-ai%2Fdsh/latest"
    request = urllib.request.Request(url, headers={"User-Agent": "dsh-desktop"})
    timeout = _environment_seconds("DSH_DESKTOP_NETWORK_TIMEOUT_SECONDS", _NETWORK_TIMEOUT_SECONDS)
    with urllib.request.urlopen(request, timeout=timeout) as response:
        data = json.load(response)
    if not isinstance(data, dict) or not isinstance(data.get("version"), str):
        raise LocalizedError("version_missing")
    version = data["version"]
    _parse_semver(version)
    return version


def _probe_registry_version(
    registry: str,
    version: str,
    controller: DeploymentController,
) -> bool:
    controller.check()
    url = f"{registry}/@deepseek-ai%2Fdsh/{version}"
    request = urllib.request.Request(url, headers={"User-Agent": "dsh-desktop"})
    timeout = _environment_seconds("DSH_DESKTOP_NETWORK_TIMEOUT_SECONDS", _NETWORK_TIMEOUT_SECONDS)
    with urllib.request.urlopen(request, timeout=timeout) as response:
        data = json.load(response)
    return isinstance(data, dict) and data.get("version") == version


def _node_manifest_checksum(
    base: str,
    version: str,
    filename: str,
    controller: DeploymentController,
) -> str:
    controller.check()
    url = f"{base}/v{version}/SHASUMS256.txt"
    request = urllib.request.Request(url, headers={"User-Agent": "dsh-desktop-release-check"})
    timeout = _environment_seconds("DSH_DESKTOP_NETWORK_TIMEOUT_SECONDS", _NETWORK_TIMEOUT_SECONDS)
    with urllib.request.urlopen(request, timeout=timeout) as response:
        content = response.read().decode("utf-8")
    for line in content.splitlines():
        fields = line.split()
        if len(fields) == 2 and fields[1] == filename and re.fullmatch(r"[0-9a-fA-F]{64}", fields[0]):
            return fields[0].lower()
    raise RuntimeError(f"{_display_source(url)} does not list {filename}")


def verify_release_sources(controller: DeploymentController | None = None) -> tuple[str, ...]:
    """Verify pinned Node metadata and valid Harness latest metadata on each default source."""
    owner = controller or DeploymentController()
    version = resolve_node_version()
    filename = node_dist_filename(version)
    expected = _node_archive_sha256(version, filename)
    checked: list[str] = []
    for base in (NODE_DIST_BASE_DEFAULT, NODE_DIST_BASE_FALLBACK):
        actual = _node_manifest_checksum(base, version, filename, owner)
        if actual != expected:
            raise RuntimeError(
                f"{_display_source(base)} lists {actual} for {filename}; expected {expected}",
            )
        checked.append(f"Node {version} via {_display_source(base)}")
    for registry in (NPM_REGISTRY_DEFAULT, NPM_REGISTRY_FALLBACK):
        harness_version = _query_registry_version(registry, owner)
        checked.append(f"Harness latest {harness_version} via {_display_source(registry)}")
    return tuple(checked)


def _ordered_install_registries(
    version: str,
    controller: DeploymentController,
) -> tuple[str, ...]:
    """Prefer registries that prove the exact package version is reachable."""
    configured = npm_registries()
    reachable: list[str] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(configured)) as executor:
        futures = {
            executor.submit(_probe_registry_version, registry, version, controller): registry
            for registry in configured
        }
        for future in concurrent.futures.as_completed(futures):
            registry = futures[future]
            try:
                if future.result():
                    reachable.append(registry)
            except DeploymentCancelled:
                raise
            except Exception:
                pass
    controller.check()
    return tuple(reachable + [registry for registry in configured if registry not in reachable])


def latest_harness_version(
    registry: str | None = None,
    controller: DeploymentController | None = None,
) -> str:
    """Highest valid latest version concurrently reachable from selected registries."""
    registries = npm_registries(registry)
    results: list[str] = []
    errors: list[str] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(registries)) as executor:
        futures = {
            executor.submit(_query_registry_version, item, controller): item
            for item in registries
        }
        for future in concurrent.futures.as_completed(futures):
            source = futures[future]
            try:
                results.append(future.result())
            except DeploymentCancelled:
                raise
            except Exception as exc:
                errors.append(f"{_display_source(source)}: {exc}")
    if controller is not None:
        controller.check()
    if not results:
        raise LocalizedError("version_query_failed", detail="; ".join(errors))
    return max(results, key=cmp_to_key(compare_versions))


def node_dist_filename(version: str) -> str:
    """The Node.js distribution filename for this OS and architecture."""
    platform_tag = node_platform()
    arch = arch_tag()
    if platform_tag == "darwin":
        return f"node-v{version}-darwin-{arch}.tar.gz"
    if platform_tag == "win":
        return f"node-v{version}-win-{arch}.zip"
    raise LocalizedError("unsupported_platform", platform=platform_tag)


def resolve_node_version() -> str:
    """Return the pinned Node version or an explicitly supplied exact version."""
    value = os.environ.get("DSH_DESKTOP_NODE_VERSION", NODE_VERSION).lstrip("v")
    try:
        _parse_semver(value)
    except ValueError as exc:
        raise LocalizedError("environment_invalid", variable="DSH_DESKTOP_NODE_VERSION", value=value) from exc
    return value


def _node_archive_sha256(version: str, filename: str) -> str:
    configured = os.environ.get("DSH_DESKTOP_NODE_SHA256")
    expected = configured if configured is not None else _NODE_ARCHIVE_SHA256.get(filename)
    if expected is None or re.fullmatch(r"[0-9a-fA-F]{64}", expected) is None:
        raise LocalizedError("node_checksum_missing", version=version, filename=filename)
    return expected.lower()


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def _response_total(response: BinaryIO, offset: int) -> int:
    content_range = response.headers.get("Content-Range")
    if content_range:
        match = re.fullmatch(r"bytes (\d+)-(\d+)/(\d+|\*)", content_range)
        if match is None or int(match.group(1)) != offset:
            raise OSError(f"unexpected Content-Range: {content_range}")
        return -1 if match.group(3) == "*" else int(match.group(3))
    length = response.headers.get("Content-Length")
    return offset + int(length) if length is not None else -1


def _download_once(
    url: str,
    partial: Path,
    on_progress: ProgressCallback,
    controller: DeploymentController,
    deadline: float,
) -> None:
    controller.check()
    offset = partial.stat().st_size if partial.is_file() else 0
    headers = {"User-Agent": "dsh-desktop"}
    if offset:
        headers["Range"] = f"bytes={offset}-"
    request = urllib.request.Request(url, headers=headers)
    timeout = _environment_seconds("DSH_DESKTOP_NETWORK_TIMEOUT_SECONDS", _NETWORK_TIMEOUT_SECONDS)
    with urllib.request.urlopen(request, timeout=timeout) as response:
        status_code = getattr(response, "status", response.getcode())
        if offset and status_code != 206:
            offset = 0
        total = _response_total(response, offset)
        mode = "ab" if offset else "wb"
        done = offset
        on_progress(done, total)
        with partial.open(mode) as handle:
            while True:
                controller.check()
                if time.monotonic() >= deadline:
                    raise TimeoutError("download exceeded its total time limit")
                chunk = response.read(_CHUNK_SIZE)
                if not chunk:
                    break
                handle.write(chunk)
                done += len(chunk)
                on_progress(done, total)
            handle.flush()
            os.fsync(handle.fileno())


def _download(
    urls: tuple[str, ...],
    destination: Path,
    expected_sha256: str,
    on_progress: ProgressCallback,
    controller: DeploymentController,
) -> None:
    """Download verified bytes with bounded retries, mirror fallback, and resume."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.is_file() and _sha256(destination) == expected_sha256:
        size = destination.stat().st_size
        on_progress(size, size)
        return
    destination.unlink(missing_ok=True)
    partial = destination.with_name(destination.name + ".part")
    deadline = time.monotonic() + _environment_seconds(
        "DSH_DESKTOP_DOWNLOAD_TIMEOUT_SECONDS",
        _DOWNLOAD_TIMEOUT_SECONDS,
    )
    errors: list[str] = []
    for attempt in range(1, _DOWNLOAD_ATTEMPTS_PER_SOURCE + 1):
        for url in urls:
            try:
                if partial.is_file() and _sha256(partial) == expected_sha256:
                    os.replace(partial, destination)
                    return
                _download_once(url, partial, on_progress, controller, deadline)
                actual = _sha256(partial)
                if actual != expected_sha256:
                    partial.unlink(missing_ok=True)
                    raise OSError(f"SHA-256 mismatch: expected {expected_sha256}, received {actual}")
                os.replace(partial, destination)
                return
            except DeploymentCancelled:
                raise
            except (urllib.error.URLError, OSError, TimeoutError, socket.timeout) as exc:
                errors.append(f"{_display_source(url)} attempt {attempt}: {exc}")
                if time.monotonic() >= deadline:
                    break
        if time.monotonic() >= deadline:
            break
        if attempt < _DOWNLOAD_ATTEMPTS_PER_SOURCE:
            controller.cancelled.wait(min(2 ** (attempt - 1), 4))
            controller.check()
    raise LocalizedError("download_failed", detail="; ".join(errors))


def _validated_member_path(name: str) -> PurePosixPath:
    normalized = PurePosixPath(name.replace("\\", "/"))
    if (
        normalized.is_absolute()
        or not normalized.parts
        or ".." in normalized.parts
        or re.fullmatch(r"[A-Za-z]:", normalized.parts[0]) is not None
    ):
        raise LocalizedError("node_archive_unsafe", entry=name)
    return normalized


def _validate_tar_members(bundle: tarfile.TarFile) -> None:
    for member in bundle.getmembers():
        path = _validated_member_path(member.name)
        if member.ischr() or member.isblk() or member.isfifo():
            raise LocalizedError("node_archive_unsafe", entry=member.name)
        if member.issym():
            target = PurePosixPath(member.linkname)
            combined = PurePosixPath(posixpath.normpath(str(path.parent / target)))
            if (
                target.is_absolute()
                or ".." in combined.parts
                or not combined.parts
                or combined.parts[0] != path.parts[0]
            ):
                raise LocalizedError("node_archive_unsafe", entry=member.name)
        if member.islnk():
            target = _validated_member_path(member.linkname)
            if target.parts[0] != path.parts[0]:
                raise LocalizedError("node_archive_unsafe", entry=member.name)


def _validate_zip_members(bundle: zipfile.ZipFile) -> None:
    for member in bundle.infolist():
        _validated_member_path(member.filename)
        mode = member.external_attr >> 16
        if stat.S_ISLNK(mode):
            raise LocalizedError("node_archive_unsafe", entry=member.filename)


def _remove_owned_path(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
        path.unlink()
    else:
        shutil.rmtree(path)


def _extract_node(archive: Path, destination: Path) -> None:
    """Validate and extract one Node distribution top-level directory."""
    _remove_owned_path(destination)
    destination.mkdir(parents=True)
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as bundle:
            _validate_zip_members(bundle)
            bundle.extractall(destination)
    else:
        with tarfile.open(archive) as bundle:
            _validate_tar_members(bundle)
            bundle.extractall(destination)
    children = list(destination.iterdir())
    if len(children) != 1 or not children[0].is_dir() or children[0].is_symlink():
        raise LocalizedError("node_archive_invalid", entries=[child.name for child in children])
    top_level = children[0]
    for child in list(top_level.iterdir()):
        child.rename(destination / child.name)
    top_level.rmdir()


def _npm_cli(node_dir: Path) -> Path:
    if os.name == "nt":
        return node_dir / "node_modules" / "npm" / "bin" / "npm-cli.js"
    return node_dir / "lib" / "node_modules" / "npm" / "bin" / "npm-cli.js"


def _node_executable(node_dir: Path) -> Path:
    return node_dir / ("node.exe" if os.name == "nt" else "bin/node")


def _subprocess_environment(paths: ApplicationPaths) -> dict[str, str]:
    allowed = {
        "COMSPEC", "LANG", "LC_ALL", "NODE_EXTRA_CA_CERTS",
        "NO_PROXY", "PATH", "SSL_CERT_DIR", "SSL_CERT_FILE", "SYSTEMROOT", "TEMP",
        "TMP", "TMPDIR", "http_proxy", "https_proxy", "no_proxy",
        "HTTP_PROXY", "HTTPS_PROXY",
    }
    environment = {key: value for key, value in os.environ.items() if key in allowed}
    environment["HOME"] = str(paths.app_home)
    environment["USERPROFILE"] = str(paths.app_home)
    if os.name == "nt":
        drive, tail = os.path.splitdrive(str(paths.app_home))
        environment["HOMEDRIVE"] = drive
        environment["HOMEPATH"] = tail
    environment["NPM_CONFIG_USERCONFIG"] = str(paths.cache_dir / "isolated-npmrc")
    environment["DSH_HOME"] = str(paths.cache_dir / "validation-home")
    environment["DSH_TELEMETRY_DISABLED"] = "1"
    return environment


def _install_dsh(
    paths: ApplicationPaths,
    node_dir: Path,
    destination: Path,
    version: str,
    install_log: Path,
    registry: str,
    controller: DeploymentController,
    timeout: float,
) -> str | None:
    """Install one exact Harness version into an isolated staging directory."""
    _remove_owned_path(destination)
    destination.mkdir(parents=True)
    (destination / "package.json").write_text(
        json.dumps({"name": "dsh-runtime", "private": True}) + "\n",
        encoding="utf-8",
    )
    command = [
        str(_node_executable(node_dir)),
        str(_npm_cli(node_dir)),
        "install",
        f"@deepseek-ai/dsh@{version}",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        "--package-lock=false",
        "--prefer-offline",
        f"--cache={paths.cache_dir / 'npm'}",
        "--fetch-retries=2",
        "--fetch-retry-factor=2",
        "--fetch-retry-mintimeout=1000",
        "--fetch-retry-maxtimeout=10000",
        "--fetch-timeout=60000",
    ]
    with install_log.open("a", encoding="utf-8") as log:
        log.write(
            f"\n===== npm install @deepseek-ai/dsh@{version} "
            f"registry={_display_source(registry)} =====\n",
        )
        log.flush()
        environment = _subprocess_environment(paths)
        environment["NPM_CONFIG_REGISTRY"] = registry
        try:
            returncode = controller.run(
                command,
                cwd=destination,
                stdout=log,
                timeout=timeout,
                environment=environment,
            )
        except subprocess.TimeoutExpired:
            log.write("npm install exceeded its time limit and was terminated\n")
            return None
    if returncode != 0:
        return None
    manifest = destination / "node_modules" / "@deepseek-ai" / "dsh" / "package.json"
    try:
        data = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return version if data.get("version") == version else None


def _fix_spawn_helper(dsh_dir: Path) -> None:
    prebuilds = dsh_dir / "node_modules" / "node-pty" / "prebuilds"
    for helper in prebuilds.rglob("spawn-helper"):
        try:
            helper.chmod(0o755)
        except OSError:
            pass


def _dsh_manifest_version(dsh_dir: Path) -> str | None:
    manifest = dsh_dir / "node_modules" / "@deepseek-ai" / "dsh" / "package.json"
    try:
        data = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    version = data.get("version")
    return version if isinstance(version, str) and _SEMVER.fullmatch(version) else None


def installed_version(paths: ApplicationPaths) -> str | None:
    """The deployed Harness version when its marker and manifest agree."""
    manifest_version = _dsh_manifest_version(paths.dsh_dir)
    try:
        marker_version = paths.version_file.read_text(encoding="utf-8").strip()
    except OSError:
        return None
    return manifest_version if manifest_version == marker_version else None


def _atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def _command_output(
    paths: ApplicationPaths,
    command: list[str],
    controller: DeploymentController | None = None,
) -> str | None:
    try:
        if controller is None:
            completed = subprocess.run(
                command,
                cwd=paths.runtime_dir,
                capture_output=True,
                timeout=_SMOKE_TIMEOUT_SECONDS,
                check=False,
                env=_subprocess_environment(paths),
            )
            output = completed.stdout.decode("utf-8", errors="replace").strip()
            return output if completed.returncode == 0 else None
        with tempfile.TemporaryFile() as output_handle:
            try:
                returncode = controller.run(
                    command,
                    cwd=paths.runtime_dir,
                    stdout=output_handle,
                    timeout=_SMOKE_TIMEOUT_SECONDS,
                    environment=_subprocess_environment(paths),
                )
                output_handle.seek(0)
                output = output_handle.read().decode("utf-8", errors="replace").strip()
            except DeploymentCancelled:
                raise
            except (OSError, subprocess.SubprocessError):
                return None
            return output if returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        return None


def _smoke(
    paths: ApplicationPaths,
    command: list[str],
    expected: str,
    controller: DeploymentController | None = None,
) -> bool:
    return _command_output(paths, command, controller) == expected


def _node_reported_version(
    paths: ApplicationPaths,
    node_dir: Path,
    controller: DeploymentController | None = None,
) -> str | None:
    output = _command_output(paths, [str(_node_executable(node_dir)), "--version"], controller)
    if output is None or not output.startswith("v"):
        return None
    version = output[1:]
    try:
        _parse_semver(version)
    except ValueError:
        return None
    return version


def _node_is_valid(paths: ApplicationPaths, node_dir: Path, controller: DeploymentController | None = None) -> bool:
    return _node_reported_version(paths, node_dir, controller) == resolve_node_version()


def _dsh_is_valid(
    paths: ApplicationPaths,
    node_dir: Path,
    dsh_dir: Path,
    version: str,
    controller: DeploymentController | None = None,
) -> bool:
    binary = dsh_dir / "node_modules" / "@deepseek-ai" / "dsh" / "lib" / "bin.js"
    if _dsh_manifest_version(dsh_dir) != version or not binary.is_file():
        return False
    return _smoke(paths, [str(_node_executable(node_dir)), str(binary), "--version"], version, controller)


def _runtime_is_valid(paths: ApplicationPaths, controller: DeploymentController | None) -> bool:
    version = installed_version(paths)
    return (
        version is not None
        and _node_is_valid(paths, paths.node_dir, controller)
        and _dsh_is_valid(paths, paths.node_dir, paths.dsh_dir, version, controller)
    )


def is_runtime_ready(paths: ApplicationPaths) -> bool:
    """Whether marker, manifests, Node, and the installed CLI all validate."""
    return _runtime_is_valid(paths, None)


def _process_exists(pid: int) -> bool:
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False
    return True


class _DeploymentLock:
    def __init__(self, path: Path, controller: DeploymentController):
        self.path = path
        self.controller = controller
        self.token = uuid.uuid4().hex
        self.acquired = False

    def _break_stale(self) -> bool:
        try:
            info = self.path.lstat()
        except FileNotFoundError:
            return True
        if stat.S_ISLNK(info.st_mode):
            self.path.unlink()
            return True
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
            pid = int(data["pid"])
        except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError):
            if time.time() - info.st_mtime > 30:
                self.path.unlink(missing_ok=True)
                return True
            return False
        if not _process_exists(pid):
            self.path.unlink(missing_ok=True)
            return True
        return False

    def __enter__(self) -> "_DeploymentLock":
        deadline = time.monotonic() + _LOCK_TIMEOUT_SECONDS
        payload = json.dumps({"pid": os.getpid(), "token": self.token}) + "\n"
        while True:
            self.controller.check()
            try:
                descriptor = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            except FileExistsError:
                if self._break_stale():
                    continue
                if time.monotonic() >= deadline:
                    raise LocalizedError("deployment_busy")
                self.controller.cancelled.wait(0.2)
                continue
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                handle.write(payload)
                handle.flush()
                os.fsync(handle.fileno())
            self.acquired = True
            return self

    def __exit__(self, _type: object, _value: object, _traceback: object) -> None:
        if not self.acquired:
            return
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
            if data.get("token") == self.token:
                self.path.unlink(missing_ok=True)
        except (OSError, json.JSONDecodeError):
            pass


def _recover_interrupted_publication(paths: ApplicationPaths) -> None:
    for name in ("node", "dsh"):
        active = paths.runtime_dir / name
        previous = paths.runtime_dir / f"{name}.previous"
        if not active.exists() and previous.is_dir() and not previous.is_symlink():
            previous.rename(active)
    for pattern in ("node.staging-*", "dsh.staging-*", "*.failed-*"):
        for path in paths.runtime_dir.glob(pattern):
            _remove_owned_path(path)


def _repair_version_marker(paths: ApplicationPaths, controller: DeploymentController) -> None:
    version = _dsh_manifest_version(paths.dsh_dir)
    if (
        version is not None
        and _node_reported_version(paths, paths.node_dir, controller) is not None
        and _dsh_is_valid(paths, paths.node_dir, paths.dsh_dir, version, controller)
    ):
        _atomic_write(paths.version_file, version + "\n")


def _runtime_pair_is_valid(
    paths: ApplicationPaths,
    node_dir: Path,
    dsh_dir: Path,
    version: str | None,
    controller: DeploymentController,
) -> bool:
    return (
        version is not None
        and node_dir.is_dir()
        and not node_dir.is_symlink()
        and dsh_dir.is_dir()
        and not dsh_dir.is_symlink()
        and _node_reported_version(paths, node_dir, controller) is not None
        and _dsh_is_valid(paths, node_dir, dsh_dir, version, controller)
    )


def _recover_valid_previous(paths: ApplicationPaths, controller: DeploymentController) -> None:
    node_previous = paths.runtime_dir / "node.previous"
    current_version = _dsh_manifest_version(paths.dsh_dir)
    dsh_previous = paths.runtime_dir / "dsh.previous"
    previous_version = _dsh_manifest_version(dsh_previous)
    active_pair_valid = _runtime_pair_is_valid(
        paths,
        paths.node_dir,
        paths.dsh_dir,
        current_version,
        controller,
    )
    if not active_pair_valid and _runtime_pair_is_valid(
        paths,
        node_previous,
        paths.dsh_dir,
        current_version,
        controller,
    ):
        _rollback_directory(paths.node_dir, node_previous)
        active_pair_valid = True
    elif not active_pair_valid and _runtime_pair_is_valid(
        paths,
        paths.node_dir,
        dsh_previous,
        previous_version,
        controller,
    ):
        _rollback_directory(paths.dsh_dir, dsh_previous)
        _atomic_write(paths.version_file, f"{previous_version}\n")
        active_pair_valid = True
    elif not active_pair_valid and _runtime_pair_is_valid(
        paths,
        node_previous,
        dsh_previous,
        previous_version,
        controller,
    ):
        _rollback_directory(paths.node_dir, node_previous)
        _rollback_directory(paths.dsh_dir, dsh_previous)
        _atomic_write(paths.version_file, f"{previous_version}\n")
        active_pair_valid = True

    if (
        not active_pair_valid
        and not _node_is_valid(paths, paths.node_dir, controller)
        and node_previous.is_dir()
        and not node_previous.is_symlink()
        and _node_is_valid(paths, node_previous, controller)
    ):
        _rollback_directory(paths.node_dir, node_previous)

    current_version = _dsh_manifest_version(paths.dsh_dir)
    current_valid = current_version is not None and _dsh_is_valid(
        paths,
        paths.node_dir,
        paths.dsh_dir,
        current_version,
        controller,
    )
    if (
        not current_valid
        and previous_version is not None
        and dsh_previous.is_dir()
        and not dsh_previous.is_symlink()
        and _dsh_is_valid(
            paths,
            paths.node_dir,
            dsh_previous,
            previous_version,
            controller,
        )
    ):
        _rollback_directory(paths.dsh_dir, dsh_previous)
        _atomic_write(paths.version_file, previous_version + "\n")


def _publish_directory(staging: Path, active: Path) -> Path | None:
    previous = active.with_name(active.name + ".previous")
    _remove_owned_path(previous)
    moved_previous = False
    if active.exists() or active.is_symlink():
        active.rename(previous)
        moved_previous = True
    try:
        staging.rename(active)
    except Exception:
        if moved_previous and not active.exists():
            previous.rename(active)
        raise
    return previous if moved_previous else None


def _rollback_directory(active: Path, previous: Path | None) -> None:
    failed = active.with_name(f"{active.name}.failed-{uuid.uuid4().hex}")
    if active.exists() or active.is_symlink():
        active.rename(failed)
    if previous is not None and previous.exists():
        previous.rename(active)
    _remove_owned_path(failed)


def _ensure_node(
    paths: ApplicationPaths,
    progress: ProgressCallback,
    controller: DeploymentController,
    on_activity: ActivityCallback,
) -> Path | None:
    if _node_is_valid(paths, paths.node_dir, controller):
        return None
    version = resolve_node_version()
    filename = node_dist_filename(version)
    expected_sha256 = _node_archive_sha256(version, filename)
    archive = paths.cache_dir / filename
    urls = tuple(f"{base}/v{version}/{filename}" for base in node_dist_bases())
    on_activity("downloading_node", {"version": version})
    _download(urls, archive, expected_sha256, progress, controller)
    on_activity("verifying_node", {"version": version})
    staging = paths.runtime_dir / f"node.staging-{uuid.uuid4().hex}"
    try:
        _extract_node(archive, staging)
        if not _node_is_valid(paths, staging, controller):
            raise LocalizedError("runtime_validation_failed", component="Node.js")
        previous = _publish_directory(staging, paths.node_dir)
        if not _node_is_valid(paths, paths.node_dir, controller):
            _rollback_directory(paths.node_dir, previous)
            raise LocalizedError("runtime_validation_failed", component="Node.js")
        return previous
    finally:
        _remove_owned_path(staging)


def _install_with_fallback(
    paths: ApplicationPaths,
    version: str,
    controller: DeploymentController,
    on_activity: ActivityCallback,
) -> Path:
    staging = paths.runtime_dir / f"dsh.staging-{uuid.uuid4().hex}"
    keep = False
    deadline = time.monotonic() + _environment_seconds(
        "DSH_DESKTOP_INSTALL_TIMEOUT_SECONDS",
        _INSTALL_TIMEOUT_SECONDS,
    )
    try:
        on_activity("checking_sources", {"version": version})
        for registry in _ordered_install_registries(version, controller):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            on_activity(
                "installing_harness",
                {"version": version, "source": _display_source(registry)},
            )
            installed = _install_dsh(
                paths,
                paths.node_dir,
                staging,
                version,
                paths.install_log,
                registry,
                controller,
                remaining,
            )
            if installed is None:
                continue
            on_activity("validating_harness", {"version": version})
            _fix_spawn_helper(staging)
            if _dsh_is_valid(paths, paths.node_dir, staging, version, controller):
                keep = True
                return staging
        raise LocalizedError("install_failed", log=paths.install_log)
    finally:
        if not keep:
            _remove_owned_path(staging)


def deploy_runtime(
    paths: ApplicationPaths,
    on_step: StepCallback,
    on_progress: ProgressCallback | None = None,
    on_activity: ActivityCallback | None = None,
    force: bool = False,
    target_version: str | None = None,
    controller: DeploymentController | None = None,
) -> Path:
    """Validate or deploy an exact runtime while reporting each blocking activity."""
    owner = controller or DeploymentController()
    paths.ensure_dirs()
    progress = on_progress or (lambda _done, _total: None)
    activity = on_activity or (lambda _key, _values: None)
    on_step("prepare")
    activity("waiting_for_lock", {})
    with _DeploymentLock(paths.deployment_lock, owner):
        activity("checking_runtime", {})
        _recover_interrupted_publication(paths)
        _recover_valid_previous(paths, owner)
        if installed_version(paths) is None:
            _repair_version_marker(paths, owner)
        if not force and _runtime_is_valid(paths, owner):
            return paths.node_bin
        previous_version = installed_version(paths)
        previous_runtime_valid = (
            previous_version is not None
            and _node_reported_version(paths, paths.node_dir, owner) is not None
            and _dsh_is_valid(
                paths,
                paths.node_dir,
                paths.dsh_dir,
                previous_version,
                owner,
            )
        )
        if target_version is None:
            activity("resolving_version", {})
            version = latest_harness_version(controller=owner)
        else:
            version = target_version
            try:
                _parse_semver(version)
            except ValueError as exc:
                raise LocalizedError("runtime_version_invalid", version=version) from exc
        node_previous: Path | None = None
        try:
            node_previous = _ensure_node(paths, progress, owner, activity)
            progress(0, -1)
            staging = _install_with_fallback(paths, version, owner, activity)
            activity("activating_harness", {"version": version})
            previous: Path | None = None
            try:
                previous = _publish_directory(staging, paths.dsh_dir)
                if not _dsh_is_valid(paths, paths.node_dir, paths.dsh_dir, version, owner):
                    raise LocalizedError("runtime_validation_failed", component="DeepSeek Harness")
                _atomic_write(paths.version_file, version + "\n")
            except Exception:
                if paths.dsh_dir.exists() or paths.dsh_dir.is_symlink():
                    _rollback_directory(paths.dsh_dir, previous)
                raise
            finally:
                _remove_owned_path(staging)
        except Exception:
            if (
                previous_runtime_valid
                and node_previous is not None
                and node_previous.exists()
            ):
                _rollback_directory(paths.node_dir, node_previous)
            raise
        return paths.node_bin
