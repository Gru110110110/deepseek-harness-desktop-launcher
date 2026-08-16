# SPDX-License-Identifier: MIT
"""Lifecycle of the ``dsh web`` service as a managed child process.

The launcher starts the service with the bundled Node binary and owns its
shutdown: an explicit application exit terminates the whole process tree so no
orphaned server survives, while hiding the desktop window leaves it running. On
POSIX the service is started in a new session so a
process-group signal reaches every descendant; on Windows the child is spawned
with a new process group and killed with ``taskkill /T``. Service output is
copied to the diagnostic log, while the official ``dsh web: <URL>`` line
provides both readiness and the address returned to the UI.
"""
from __future__ import annotations

import json
import os
import queue
import shutil
import signal
import subprocess
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, TextIO
from urllib.parse import urlsplit

from app_paths import ApplicationPaths
from cc_switch_import import import_cc_switch_configuration
from localization import LocalizedText, message

# A ready callback receives the canonical Web URL; an error callback receives
# deferred copy so the UI can render its current language.
ReadyCallback = Callable[[str], None]
ErrorCallback = Callable[[LocalizedText], None]

_READY_TIMEOUT_SECONDS = 60.0
_TERMINATE_GRACE_SECONDS = 8.0
_WEB_URL_PREFIX = "dsh web: "
_DESTINATION_RUNTIME_ENTRIES = frozenset({
    ".anonymous-user-id",
    "attachments",
    "sessions",
    "storages",
})
_SNAPSHOT_EXCLUDED_ENTRIES = frozenset({
    ".anonymous-user-id",
    "storages",
})
_HISTORY_ENTRIES = frozenset({"attachments", "sessions"})
_NON_CONFIG_SUFFIXES = (".lock", ".tmp")
_HOME_IMPORT_MARKER_CONTENT = "1\n"
_WORKSPACE_STORAGE_RELATIVE_PATH = Path("storages") / "workspace.json"
_WORKSPACE_STORAGE_UNIT = {"name": "workspace", "version": 2}
_SOURCE_HOME_ENV = "DSH_DESKTOP_SOURCE_HOME"
_CC_SWITCH_HOME_ENV = "DSH_DESKTOP_CC_SWITCH_HOME"


def _service_command(paths: ApplicationPaths, *, use_free_port: bool = False) -> list[str]:
    """Build the official Web command for its default or an OS-assigned port."""
    command = [str(paths.node_bin), str(paths.dsh_bin), "web"]
    if use_free_port:
        command.extend(("--port", "0"))
    return command


def _source_home_from_environment(environment: dict[str, str]) -> Path:
    """Resolve the optional import source without changing the process home."""
    configured = environment.get(_SOURCE_HOME_ENV)
    return Path(configured).expanduser() if configured else Path.home() / ".dsh"


def _cc_switch_home_from_environment(environment: dict[str, str]) -> Path:
    """Resolve the read-only CC Switch source without changing process home."""
    configured = environment.get(_CC_SWITCH_HOME_ENV)
    return Path(configured).expanduser() if configured else Path.home() / ".cc-switch"


@dataclass(frozen=True)
class _StartupResult:
    """Readiness outcome and whether the official default port was occupied."""

    web_url: str | None
    address_in_use: bool


@dataclass(frozen=True)
class _HomeImportResult:
    """One source-home import outcome rendered into the desktop diagnostic log."""

    copied: bool
    message: str


@dataclass(frozen=True)
class _WorkspaceRegistrySnapshot:
    """Validated workspace v2 bytes and whether they contain user state."""

    content: bytes
    empty: bool


def _windows_hidden_options() -> dict[str, object]:
    # CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
    return {"creationflags": 0x00000200 | 0x08000000}


def _launch_options() -> dict[str, object]:
    if os.name == "nt":
        return _windows_hidden_options()
    return {"start_new_session": True}


def _configuration_entry_name(path: Path) -> bool:
    """Whether a Harness-home entry name denotes configuration rather than runtime state."""
    return (
        path.name not in _DESTINATION_RUNTIME_ENTRIES
        and path.name != "node_modules"
        and not path.name.endswith(_NON_CONFIG_SUFFIXES)
    )


def _eligible_source_entry(path: Path) -> bool:
    """Whether a source entry can be materialized in the configuration snapshot."""
    return (
        path.name not in _SNAPSHOT_EXCLUDED_ENTRIES
        and path.name != "node_modules"
        and not path.name.endswith(_NON_CONFIG_SUFFIXES)
        and not path.is_symlink()
        and (path.is_file() or path.is_dir())
    )


def _ignore_nonportable_entries(directory: str, names: list[str]) -> set[str]:
    """Exclude dependency installations and links from a copied config tree."""
    root = Path(directory)
    return {
        name
        for name in names
        if name == "node_modules" or (root / name).is_symlink()
    }


def _remove_published_entry(path: Path) -> None:
    """Remove one migration-owned destination during rollback."""
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink(missing_ok=True)


def _publish_staged_entry(source: Path, target: Path) -> None:
    """Publish one staged tree without replacing any concurrently created path."""
    if source.is_dir():
        target.mkdir()
        try:
            for child in source.iterdir():
                _publish_staged_entry(child, target / child.name)
            shutil.copystat(source, target, follow_symlinks=False)
        except OSError:
            _remove_published_entry(target)
            raise
        return

    created = False
    try:
        with source.open("rb") as source_file, target.open("xb") as target_file:
            created = True
            shutil.copyfileobj(source_file, target_file)
        shutil.copystat(source, target, follow_symlinks=False)
    except OSError:
        if created:
            target.unlink(missing_ok=True)
        raise


def _missing_publication_roots(source: Path, destination: Path) -> list[tuple[Path, Path]]:
    """Find maximal staged subtrees whose destination paths do not exist."""
    if destination.exists() or destination.is_symlink():
        if source.is_dir() and destination.is_dir() and not destination.is_symlink():
            roots: list[tuple[Path, Path]] = []
            for child in source.iterdir():
                roots.extend(_missing_publication_roots(child, destination / child.name))
            return roots
        return []
    return [(source, destination)]


def _has_configuration(home: Path) -> bool:
    """Whether a desktop home contains configuration rather than runtime-only state."""
    return home.is_dir() and any(_configuration_entry_name(entry) for entry in home.iterdir())


def _marker_is_complete(marker: Path) -> bool:
    """Whether the versioned one-time import marker is complete and readable."""
    try:
        return marker.read_text(encoding="utf-8") == _HOME_IMPORT_MARKER_CONTENT
    except OSError:
        return False


def _write_import_marker(marker: Path) -> None:
    """Atomically record completion of the versioned source-home import."""
    marker.parent.mkdir(parents=True, exist_ok=True)
    temporary = marker.with_name(f"{marker.name}.tmp")
    try:
        temporary.write_text(_HOME_IMPORT_MARKER_CONTENT, encoding="utf-8")
        temporary.replace(marker)
    except OSError:
        temporary.unlink(missing_ok=True)
        raise


def _read_workspace_registry(path: Path) -> _WorkspaceRegistrySnapshot | None:
    """Read the portable workspace v2 fields without accepting links or partial state."""
    if path.is_symlink() or not path.is_file():
        return None
    try:
        content = path.read_bytes()
        document = json.loads(content)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError):
        return None
    if not isinstance(document, dict) or document.get("unit") != _WORKSPACE_STORAGE_UNIT:
        return None
    global_state = document.get("global")
    tables = document.get("tables")
    if not isinstance(global_state, dict) or not isinstance(tables, dict):
        return None
    workspace_ids = global_state.get("workspaceIds")
    archived_ids = global_state.get("archivedSessionIds", [])
    workspaces = tables.get("workspaces")
    if (
        global_state.get("initialized") is not True
        or "pendingMutation" in global_state
        or not isinstance(workspace_ids, list)
        or not all(isinstance(item, str) for item in workspace_ids)
        or not isinstance(archived_ids, list)
        or not all(isinstance(item, str) for item in archived_ids)
        or not isinstance(workspaces, dict)
        or not all(isinstance(item, str) for item in workspaces)
        or len(workspace_ids) != len(set(workspace_ids))
        or set(workspace_ids) != set(workspaces)
    ):
        return None
    for record in workspaces.values():
        if not isinstance(record, dict):
            return None
        session_ids = record.get("sessionIds")
        if (
            not isinstance(record.get("path"), str)
            or not isinstance(record.get("title"), str)
            or not isinstance(session_ids, list)
            or not all(isinstance(item, str) for item in session_ids)
            or not isinstance(record.get("createdAt"), str)
            or not isinstance(record.get("updatedAt"), str)
        ):
            return None
    return _WorkspaceRegistrySnapshot(
        content=content,
        empty=not workspace_ids and not archived_ids,
    )


def _import_source_workspace(
    source_home: Path,
    destination_home: Path,
    marker: Path,
) -> _HomeImportResult:
    """Import a compatible workspace ledger when desktop grouping is absent.

    A populated or unrecognized desktop ledger always wins. The one repairable
    existing file is a validated workspace v2 ledger with no workspaces and no
    archived sessions, which the launcher may have initialized before source
    sessions arrived. No other storage-domain file participates.
    """
    source = source_home.expanduser().resolve()
    destination = destination_home.expanduser().resolve()
    if source == destination or source in destination.parents:
        return _HomeImportResult(False, "workspace skipped because source and destination overlap")
    if _marker_is_complete(marker):
        return _HomeImportResult(False, "workspace skipped because source-workspace import v1 is already complete")
    if not source.is_dir():
        return _HomeImportResult(False, "workspace skipped because the source home does not exist")

    source_path = source / _WORKSPACE_STORAGE_RELATIVE_PATH
    if source_path.parent.is_symlink():
        _write_import_marker(marker)
        return _HomeImportResult(False, "workspace completed with no compatible source grouping to import")
    source_snapshot = _read_workspace_registry(source_path)
    if source_snapshot is None or source_snapshot.empty:
        _write_import_marker(marker)
        return _HomeImportResult(False, "workspace completed with no compatible source grouping to import")

    destination.mkdir(parents=True, exist_ok=True)
    destination_path = destination / _WORKSPACE_STORAGE_RELATIVE_PATH
    storage_dir = destination_path.parent
    if storage_dir.is_symlink() or (storage_dir.exists() and not storage_dir.is_dir()):
        _write_import_marker(marker)
        return _HomeImportResult(False, "workspace completed with existing desktop storage preserved")
    destination_exists = destination_path.exists() or destination_path.is_symlink()
    destination_snapshot = _read_workspace_registry(destination_path)
    if destination_exists and (destination_snapshot is None or not destination_snapshot.empty):
        _write_import_marker(marker)
        return _HomeImportResult(False, "workspace completed with existing desktop grouping preserved")

    storage_dir_created = False
    published = False
    replaced = False
    with tempfile.TemporaryDirectory(prefix=".dsh-workspace-import-", dir=destination.parent) as temporary:
        staging = Path(temporary)
        staged = staging / "workspace.json"
        staged.write_bytes(source_snapshot.content)
        shutil.copystat(source_path, staged, follow_symlinks=False)
        if _read_workspace_registry(staged) is None:
            raise OSError("source workspace grouping changed during import")
        backup = staging / "desktop-workspace.json"
        try:
            if not storage_dir.exists():
                storage_dir.mkdir()
                storage_dir_created = True
            current_exists = destination_path.exists() or destination_path.is_symlink()
            current_snapshot = _read_workspace_registry(destination_path)
            if current_exists and (current_snapshot is None or not current_snapshot.empty):
                _write_import_marker(marker)
                return _HomeImportResult(False, "workspace completed with concurrent desktop grouping preserved")
            if current_snapshot is not None:
                backup.write_bytes(current_snapshot.content)
                shutil.copystat(destination_path, backup, follow_symlinks=False)
                staged.replace(destination_path)
                replaced = True
            else:
                _publish_staged_entry(staged, destination_path)
                published = True
            _write_import_marker(marker)
        except OSError:
            if published:
                destination_path.unlink(missing_ok=True)
            elif replaced:
                backup.replace(destination_path)
            if storage_dir_created:
                try:
                    storage_dir.rmdir()
                except OSError:
                    # A concurrent writer owns the non-empty storage directory.
                    pass
            raise
    return _HomeImportResult(
        True,
        "workspace completed: grouping=repaired" if destination_exists else "workspace completed: grouping=copied",
    )


def _import_source_home(
    source_home: Path,
    destination_home: Path,
    marker: Path,
) -> _HomeImportResult:
    """Import missing configuration and an absent history pair exactly once.

    Existing destination paths always win. Configuration directories merge only at
    missing descendants. Sessions and attachments copy only when neither destination
    history directory exists. A completed marker suppresses later imports while the
    destination still has configuration; clearing all configuration makes it eligible
    again. Other runtime data, dependencies, and symbolic links stay behind.
    """
    source = source_home.expanduser().resolve()
    destination = destination_home.expanduser().resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.mkdir(parents=True, exist_ok=True)

    if source == destination or source in destination.parents:
        return _HomeImportResult(False, "skipped because source and destination overlap")
    if _marker_is_complete(marker) and _has_configuration(destination):
        return _HomeImportResult(False, "skipped because source-home import v1 is already complete")
    if not source.is_dir():
        return _HomeImportResult(False, "skipped because the source home does not exist")

    destination_has_history = any(
        (destination / name).exists() or (destination / name).is_symlink()
        for name in _HISTORY_ENTRIES
    )
    source_entries = [
        entry
        for entry in source.iterdir()
        if _eligible_source_entry(entry)
        and not (destination_has_history and entry.name in _HISTORY_ENTRIES)
    ]
    if not source_entries:
        if destination_has_history:
            _write_import_marker(marker)
            return _HomeImportResult(False, "completed with existing desktop history preserved; source has no configuration to add")
        return _HomeImportResult(False, "skipped because the source home has no portable data")

    published: list[Path] = []
    configuration_count = 0
    history_count = 0
    with tempfile.TemporaryDirectory(prefix=".dsh-config-import-", dir=destination.parent) as temporary:
        staging = Path(temporary)
        for entry in source_entries:
            staged = staging / entry.name
            if entry.is_dir():
                shutil.copytree(entry, staged, ignore=_ignore_nonportable_entries)
            else:
                shutil.copy2(entry, staged)
        try:
            for staged in staging.iterdir():
                roots = _missing_publication_roots(staged, destination / staged.name)
                for source_root, target_root in roots:
                    _publish_staged_entry(source_root, target_root)
                    published.append(target_root)
                    if staged.name in _HISTORY_ENTRIES:
                        history_count += 1
                    else:
                        configuration_count += 1
        except OSError:
            for target in reversed(published):
                _remove_published_entry(target)
            raise
    _write_import_marker(marker)
    history = "preserved" if destination_has_history else "copied" if history_count > 0 else "absent"
    return _HomeImportResult(
        len(published) > 0,
        f"completed: configuration entries copied={configuration_count}; history={history}",
    )


def _log_home_import(paths: ApplicationPaths, message: str) -> None:
    """Append one content-free source-home import diagnostic."""
    paths.server_log.parent.mkdir(parents=True, exist_ok=True)
    with paths.server_log.open("a", encoding="utf-8") as log:
        log.write(f"desktop home import: {message}\n")


def _log_cc_switch_import(paths: ApplicationPaths, detail: str) -> None:
    """Append one diagnostic that never includes provider values or credentials."""
    paths.server_log.parent.mkdir(parents=True, exist_ok=True)
    with paths.server_log.open("a", encoding="utf-8") as log:
        log.write(f"CC Switch import: {detail}\n")


def _service_environment(
    paths: ApplicationPaths,
    inherited: dict[str, str],
    source_home: Path,
    cc_switch_home: Path,
) -> dict[str, str]:
    """Resolve service environment and perform the default-home first-run import."""
    environment = inherited.copy()
    if environment.get("DSH_HOME"):
        _log_home_import(paths, "skipped because DSH_HOME is explicit")
        return environment
    result = _import_source_home(source_home, paths.dsh_home, paths.home_import_marker)
    _log_home_import(paths, result.message)
    workspace_result = _import_source_workspace(
        source_home,
        paths.dsh_home,
        paths.workspace_import_marker,
    )
    _log_home_import(paths, workspace_result.message)
    try:
        cc_switch_result = import_cc_switch_configuration(
            cc_switch_home,
            paths.dsh_home,
            paths.cc_switch_import_marker,
        )
        _log_cc_switch_import(paths, cc_switch_result.message)
    except OSError:
        # CC Switch is an optional read-only source. Publication rolls back all
        # importer-owned files, so a failure must not strand an otherwise valid
        # desktop home or prevent the existing runtime from starting.
        _log_cc_switch_import(paths, "failed safely; existing desktop configuration preserved")
    environment["DSH_HOME"] = str(paths.dsh_home)
    return environment


def _parse_web_url(line: str) -> str | None:
    """Return the canonical URL from an official ``dsh web:`` readiness line."""
    stripped = line.strip()
    if not stripped.startswith(_WEB_URL_PREFIX):
        return None
    candidate = stripped[len(_WEB_URL_PREFIX):].split(maxsplit=1)[0]
    try:
        parsed = urlsplit(candidate)
        parsed.port
    except ValueError:
        return None
    if parsed.scheme not in {"http", "https"} or parsed.hostname is None:
        return None
    return candidate


def _capture_output(stream: TextIO, log: TextIO, output: queue.Queue[str]) -> None:
    """Tee service output to the diagnostic log and the readiness consumer."""
    logging_enabled = True
    try:
        for line in stream:
            output.put(line)
            if logging_enabled:
                try:
                    log.write(line)
                    log.flush()
                except OSError:
                    # Diagnostic logging must not hide an official readiness line.
                    logging_enabled = False
    finally:
        log.close()


class ServerManager:
    """Spawns and terminates the ``dsh web`` service process."""

    def __init__(self, paths: ApplicationPaths):
        self.paths = paths
        self.process: subprocess.Popen[str] | None = None
        self.web_url: str | None = None
        self._output_thread: threading.Thread | None = None

    @property
    def is_running(self) -> bool:
        return self.process is not None and self.process.poll() is None

    def start(self, on_ready: ReadyCallback, on_error: ErrorCallback) -> None:
        """Spawn the service and report readiness or failure via callbacks.

        Called from a worker thread; callbacks post to the tkinter queue.
        """
        if self.is_running and self.web_url is not None:
            on_ready(self.web_url)
            return
        if self.is_running:
            on_error(message("service_starting_no_address"))
            return

        try:
            inherited = dict(os.environ)
            environment = _service_environment(
                self.paths,
                inherited,
                _source_home_from_environment(inherited),
                _cc_switch_home_from_environment(inherited),
            )
        except OSError as exc:
            on_error(message("config_import_failed", detail=exc))
            return
        default_port_was_occupied = False
        for use_free_port in (False, True):
            if use_free_port and not default_port_was_occupied:
                break
            log_handle = self.paths.server_log.open("a", encoding="utf-8")
            try:
                self.process = subprocess.Popen(
                    _service_command(self.paths, use_free_port=use_free_port),
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    stdin=subprocess.DEVNULL,
                    text=True,
                    bufsize=1,
                    env=environment,
                    **(_launch_options()),
                )
            except OSError as exc:
                log_handle.close()
                on_error(message("service_start_failed", detail=exc))
                return

            self.paths.server_pid.write_text(str(self.process.pid), encoding="utf-8")
            output: queue.Queue[str] = queue.Queue()
            stdout = self.process.stdout
            if stdout is None:
                log_handle.close()
                self.stop()
                on_error(message("service_output_unreadable"))
                return
            self._output_thread = threading.Thread(
                target=_capture_output,
                args=(stdout, log_handle, output),
                name="dsh-web-output",
                daemon=True,
            )
            self._output_thread.start()

            result = self._wait_for_web_url(output)
            if result.web_url is not None:
                self.web_url = result.web_url
                on_ready(result.web_url)
                return
            self.stop()
            if not use_free_port:
                default_port_was_occupied = result.address_in_use

        if default_port_was_occupied:
            on_error(message("free_port_failed"))
        else:
            on_error(message("service_no_address"))

    def _wait_for_web_url(self, output: queue.Queue[str]) -> _StartupResult:
        """Wait for the official URL line and classify a default-port conflict."""
        deadline = time.monotonic() + _READY_TIMEOUT_SECONDS
        address_in_use = False
        while time.monotonic() < deadline:
            try:
                line = output.get(timeout=min(0.2, max(0.0, deadline - time.monotonic())))
            except queue.Empty:
                if self.process is None or self.process.poll() is not None:
                    if self._output_thread is not None:
                        self._output_thread.join(timeout=1)
                    while True:
                        try:
                            line = output.get_nowait()
                        except queue.Empty:
                            break
                        address_in_use = address_in_use or "EADDRINUSE" in line
                    return _StartupResult(None, address_in_use)
                continue
            address_in_use = address_in_use or "EADDRINUSE" in line
            web_url = _parse_web_url(line)
            if web_url is not None:
                if self.process is not None and self.process.poll() is None:
                    return _StartupResult(web_url, address_in_use)
                return _StartupResult(None, address_in_use)
        return _StartupResult(None, address_in_use)

    def stop(self) -> None:
        """Terminate the service process tree and wait for it to exit."""
        if self.process is None:
            self.web_url = None
            return
        if self.process.poll() is None:
            _terminate(self.process.pid)
            try:
                self.process.wait(timeout=_TERMINATE_GRACE_SECONDS)
            except subprocess.TimeoutExpired:
                _terminate(self.process.pid, force=True)
        if self._output_thread is not None:
            self._output_thread.join(timeout=1)
        self.process = None
        self.web_url = None
        self._output_thread = None
        self.paths.server_pid.unlink(missing_ok=True)


def _terminate(pid: int, force: bool = False) -> None:
    if pid <= 1:
        return
    try:
        if os.name == "nt":
            subprocess.run(
                ["taskkill", "/PID", str(pid), "/T", "/F"],
                capture_output=True,
                timeout=10,
                check=False,
                **(_windows_hidden_options()),
            )
        else:
            os.killpg(pid, signal.SIGKILL if force else signal.SIGTERM)
    except (OSError, subprocess.SubprocessError):
        pass
