# SPDX-License-Identifier: MIT
"""Lifecycle tests for the service process manager."""
from __future__ import annotations

import io
import json
import os
import queue
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import cast
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from app_paths import ApplicationPaths
from cc_switch_import import CcSwitchImportResult
from server_manager import (
    ServerManager,
    _cc_switch_home_from_environment,
    _capture_output,
    _import_source_home,
    _import_source_workspace,
    _launch_options,
    _parse_web_url,
    _publish_staged_entry,
    _service_environment,
    _service_command,
    _source_home_from_environment,
    _terminate,
)


def _write_workspace_registry(path: Path, workspace_count: int) -> bytes:
    """Write a valid workspace v2 fixture and return its serialized bytes."""
    workspace_ids = [f"workspace-{index}" for index in range(workspace_count)]
    document = {
        "unit": {"name": "workspace", "version": 2},
        "global": {
            "initialized": True,
            "workspaceIds": workspace_ids,
            "archivedSessionIds": [],
        },
        "tables": {
            "workspaces": {
                workspace_id: {
                    "path": f"/project/{index}",
                    "title": f"Project {index}",
                    "sessionIds": [f"session-{index}"],
                    "createdAt": "2026-08-16T00:00:00.000Z",
                    "updatedAt": "2026-08-16T00:00:00.000Z",
                }
                for index, workspace_id in enumerate(workspace_ids)
            },
        },
    }
    content = (json.dumps(document, indent=2) + "\n").encode()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return content


class _RunningProcess:
    def poll(self) -> None:
        return None


class _ExitedProcess:
    def poll(self) -> int:
        return 1


class ReadinessOutputTest(unittest.TestCase):
    def test_running_service_without_an_address_reports_deferred_copy(self) -> None:
        errors = []
        with tempfile.TemporaryDirectory() as tmp:
            manager = ServerManager(ApplicationPaths.from_home(Path(tmp)))
            manager.process = cast(subprocess.Popen[str], _RunningProcess())
            manager.start(lambda _url: self.fail("service unexpectedly ready"), errors.append)
        self.assertEqual(len(errors), 1)
        self.assertEqual(errors[0].key, "service_starting_no_address")

    def test_service_command_uses_official_default_before_free_port_retry(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp))
            self.assertEqual(
                _service_command(paths),
                [str(paths.node_bin), str(paths.dsh_bin), "web"],
            )
            self.assertEqual(
                _service_command(paths, use_free_port=True),
                [str(paths.node_bin), str(paths.dsh_bin), "web", "--port", "0"],
            )

    def test_parse_web_url_uses_official_dynamic_address(self) -> None:
        self.assertEqual(
            _parse_web_url("dsh web: http://127.0.0.1:49152 (LAN: http://192.168.1.5:49152)\n"),
            "http://127.0.0.1:49152",
        )

    def test_parse_web_url_rejects_noise_and_unsupported_schemes(self) -> None:
        self.assertIsNone(_parse_web_url("server listening on http://127.0.0.1:3080"))
        self.assertIsNone(_parse_web_url("dsh web: file:///tmp/index.html"))
        self.assertIsNone(_parse_web_url("dsh web: http://127.0.0.1:not-a-port"))

    def test_capture_output_preserves_log_and_readiness_line(self) -> None:
        captured: queue.Queue[str] = queue.Queue()
        with tempfile.TemporaryDirectory() as tmp:
            log_path = Path(tmp) / "server.log"
            log = log_path.open("a", encoding="utf-8")
            _capture_output(
                io.StringIO("booting\ndsh web: http://127.0.0.1:45678\n"),
                log,
                captured,
            )
            self.assertEqual(
                log_path.read_text(encoding="utf-8"),
                "booting\ndsh web: http://127.0.0.1:45678\n",
            )
        self.assertEqual(captured.get_nowait(), "booting\n")
        self.assertEqual(
            _parse_web_url(captured.get_nowait()),
            "http://127.0.0.1:45678",
        )

    def test_wait_for_web_url_returns_the_announced_address(self) -> None:
        output: queue.Queue[str] = queue.Queue()
        output.put("initializing\n")
        output.put("dsh web: http://localhost:41873\n")
        with tempfile.TemporaryDirectory() as tmp:
            manager = ServerManager(ApplicationPaths.from_home(Path(tmp)))
            manager.process = cast(subprocess.Popen[str], _RunningProcess())
            self.assertEqual(
                manager._wait_for_web_url(output).web_url,
                "http://localhost:41873",
            )

    def test_wait_for_web_url_classifies_an_occupied_address(self) -> None:
        output: queue.Queue[str] = queue.Queue()
        output.put("Error: listen EADDRINUSE: address already in use 127.0.0.1:3080\n")
        with tempfile.TemporaryDirectory() as tmp:
            manager = ServerManager(ApplicationPaths.from_home(Path(tmp)))
            manager.process = cast(subprocess.Popen[str], _ExitedProcess())
            result = manager._wait_for_web_url(output)
        self.assertIsNone(result.web_url)
        self.assertTrue(result.address_in_use)


class SourceHomeImportTest(unittest.TestCase):
    def test_source_home_environment_override_does_not_change_process_home(self) -> None:
        configured = "/private/tmp/dsh-launcher-source"

        self.assertEqual(
            _source_home_from_environment({"DSH_DESKTOP_SOURCE_HOME": configured}),
            Path(configured),
        )

    def test_cc_switch_environment_override_is_an_isolated_read_only_source(self) -> None:
        configured = "/private/tmp/dsh-launcher-cc-switch"

        self.assertEqual(
            _cc_switch_home_from_environment({"DSH_DESKTOP_CC_SWITCH_HOME": configured}),
            Path(configured),
        )

    def test_imports_configuration_without_runtime_data_or_dependencies(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            (source / "profiles" / "web" / "node_modules" / "plugin").mkdir(parents=True)
            (source / "profiles" / "web" / "cordis.patch.yml").write_text("[]\n", encoding="utf-8")
            (source / "profiles" / "web" / "node_modules" / "plugin" / "index.js").write_text(
                "installed dependency\n",
                encoding="utf-8",
            )
            (source / "skills" / "mine").mkdir(parents=True)
            (source / "skills" / "mine" / "SKILL.md").write_text("personal skill\n", encoding="utf-8")
            (source / "settings.yaml").write_text("theme: dark\n", encoding="utf-8")
            (source / ".credentials.yaml").write_text("DEEPSEEK_API_KEY: secret\n", encoding="utf-8")
            (source / "settings.yaml.lock").write_text("writer\n", encoding="utf-8")
            (source / "settings.yaml.123.tmp").write_text("temporary\n", encoding="utf-8")
            for runtime_dir in ("attachments", "sessions", "storages"):
                (source / runtime_dir).mkdir()
                (source / runtime_dir / "state").write_text("runtime\n", encoding="utf-8")
            (source / ".anonymous-user-id").write_text("identity\n", encoding="utf-8")
            if os.name != "nt":
                (source / "linked-settings.yaml").symlink_to(source / "settings.yaml")

            result = _import_source_home(source, destination, root / "marker")

            self.assertTrue(result.copied)
            self.assertIn("history=copied", result.message)
            self.assertEqual((destination / "settings.yaml").read_text(encoding="utf-8"), "theme: dark\n")
            self.assertEqual(
                (destination / ".credentials.yaml").read_text(encoding="utf-8"),
                "DEEPSEEK_API_KEY: secret\n",
            )
            self.assertTrue((destination / "profiles" / "web" / "cordis.patch.yml").is_file())
            self.assertTrue((destination / "skills" / "mine" / "SKILL.md").is_file())
            self.assertEqual(
                (destination / "sessions" / "state").read_text(encoding="utf-8"),
                "runtime\n",
            )
            self.assertEqual(
                (destination / "attachments" / "state").read_text(encoding="utf-8"),
                "runtime\n",
            )
            self.assertFalse((destination / "profiles" / "web" / "node_modules").exists())
            for runtime_entry in (".anonymous-user-id", "storages"):
                self.assertFalse((destination / runtime_entry).exists())
            self.assertFalse((destination / "settings.yaml.lock").exists())
            self.assertFalse((destination / "settings.yaml.123.tmp").exists())
            if os.name != "nt":
                self.assertFalse((destination / "linked-settings.yaml").exists())

    def test_runtime_only_destination_is_still_unconfigured(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            source.mkdir()
            (source / "settings.yaml").write_text("theme: dark\n", encoding="utf-8")
            (source / "sessions").mkdir()
            (source / "sessions" / "source.jsonl").write_text("source session\n", encoding="utf-8")
            (source / "attachments").mkdir()
            (source / "attachments" / "source.png").write_text("source attachment\n", encoding="utf-8")
            (destination / "sessions").mkdir(parents=True)
            (destination / "sessions" / "existing.jsonl").write_text("session\n", encoding="utf-8")

            result = _import_source_home(source, destination, root / "marker")

            self.assertTrue(result.copied)
            self.assertIn("history=preserved", result.message)
            self.assertTrue((destination / "settings.yaml").is_file())
            self.assertTrue((destination / "sessions" / "existing.jsonl").is_file())
            self.assertFalse((destination / "sessions" / "source.jsonl").exists())
            self.assertFalse((destination / "attachments").exists())

    def test_existing_configuration_wins_while_missing_entries_import(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            source.mkdir()
            destination.mkdir()
            (source / "settings.yaml").write_text("theme: source\n", encoding="utf-8")
            (source / "AGENTS.md").write_text("source instructions\n", encoding="utf-8")
            (destination / "settings.yaml").write_text("theme: desktop\n", encoding="utf-8")

            result = _import_source_home(source, destination, root / "marker")

            self.assertTrue(result.copied)
            self.assertEqual((destination / "settings.yaml").read_text(encoding="utf-8"), "theme: desktop\n")
            self.assertEqual((destination / "AGENTS.md").read_text(encoding="utf-8"), "source instructions\n")

    def test_existing_configuration_directories_merge_only_missing_descendants(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            (source / "profiles" / "web").mkdir(parents=True)
            (destination / "profiles" / "web").mkdir(parents=True)
            (source / "profiles" / "web" / "shared.yml").write_text("source\n", encoding="utf-8")
            (destination / "profiles" / "web" / "shared.yml").write_text("desktop\n", encoding="utf-8")
            (source / "profiles" / "extra" / "nested").mkdir(parents=True)
            (source / "profiles" / "extra" / "nested" / "config.yml").write_text("extra\n", encoding="utf-8")

            result = _import_source_home(source, destination, root / "marker")

            self.assertTrue(result.copied)
            self.assertEqual(
                (destination / "profiles" / "web" / "shared.yml").read_text(encoding="utf-8"),
                "desktop\n",
            )
            self.assertEqual(
                (destination / "profiles" / "extra" / "nested" / "config.yml").read_text(encoding="utf-8"),
                "extra\n",
            )

    @unittest.skipIf(os.name == "nt", "creating symlinks may require Windows Developer Mode")
    def test_existing_destination_symlink_suppresses_the_import(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            source.mkdir()
            destination.mkdir()
            (source / "settings.yaml").write_text("theme: source\n", encoding="utf-8")
            external = root / "external-settings.yaml"
            external.write_text("theme: desktop\n", encoding="utf-8")
            (destination / "settings.yaml").symlink_to(external)

            result = _import_source_home(source, destination, root / "marker")

            self.assertFalse(result.copied)
            self.assertEqual(external.read_text(encoding="utf-8"), "theme: desktop\n")

    def test_publish_failure_rolls_back_every_imported_entry(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            source.mkdir()
            (source / "settings.yaml").write_text("theme: source\n", encoding="utf-8")
            (source / "AGENTS.md").write_text("instructions\n", encoding="utf-8")
            calls = 0

            def fail_second(staged: Path, target: Path) -> None:
                nonlocal calls
                calls += 1
                if calls == 2:
                    raise OSError("simulated publication failure")
                _publish_staged_entry(staged, target)

            with patch("server_manager._publish_staged_entry", side_effect=fail_second):
                with self.assertRaisesRegex(OSError, "simulated publication failure"):
                    _import_source_home(source, destination, root / "marker")

            self.assertEqual(list(destination.iterdir()), [])
            self.assertFalse((root / "marker").exists())

    def test_workspace_import_copies_only_the_compatible_grouping_ledger(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            expected = _write_workspace_registry(source / "storages" / "workspace.json", 2)
            (source / "storages" / "session_projcache.json").write_text("cache\n", encoding="utf-8")

            result = _import_source_workspace(source, destination, root / "marker")

            self.assertTrue(result.copied)
            self.assertIn("grouping=copied", result.message)
            self.assertEqual((destination / "storages" / "workspace.json").read_bytes(), expected)
            self.assertFalse((destination / "storages" / "session_projcache.json").exists())
            self.assertEqual((root / "marker").read_text(encoding="utf-8"), "1\n")

    def test_workspace_import_repairs_an_initialized_empty_desktop_ledger(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            expected = _write_workspace_registry(source / "storages" / "workspace.json", 2)
            _write_workspace_registry(destination / "storages" / "workspace.json", 0)

            result = _import_source_workspace(source, destination, root / "marker")

            self.assertTrue(result.copied)
            self.assertIn("grouping=repaired", result.message)
            self.assertEqual((destination / "storages" / "workspace.json").read_bytes(), expected)

    def test_workspace_import_preserves_populated_desktop_grouping(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            _write_workspace_registry(source / "storages" / "workspace.json", 2)
            expected = _write_workspace_registry(destination / "storages" / "workspace.json", 1)

            result = _import_source_workspace(source, destination, root / "marker")

            self.assertFalse(result.copied)
            self.assertIn("preserved", result.message)
            self.assertEqual((destination / "storages" / "workspace.json").read_bytes(), expected)
            self.assertEqual((root / "marker").read_text(encoding="utf-8"), "1\n")

    def test_workspace_import_does_not_sync_source_changes_after_completion(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            expected = _write_workspace_registry(source / "storages" / "workspace.json", 1)

            _import_source_workspace(source, destination, root / "marker")
            _write_workspace_registry(source / "storages" / "workspace.json", 2)
            result = _import_source_workspace(source, destination, root / "marker")

            self.assertFalse(result.copied)
            self.assertIn("already complete", result.message)
            self.assertEqual((destination / "storages" / "workspace.json").read_bytes(), expected)

    def test_workspace_import_rejects_an_unknown_storage_version(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            source_path = source / "storages" / "workspace.json"
            _write_workspace_registry(source_path, 1)
            document = json.loads(source_path.read_text(encoding="utf-8"))
            document["unit"]["version"] = 3
            source_path.write_text(json.dumps(document), encoding="utf-8")

            result = _import_source_workspace(source, destination, root / "marker")

            self.assertFalse(result.copied)
            self.assertFalse((destination / "storages" / "workspace.json").exists())
            self.assertEqual((root / "marker").read_text(encoding="utf-8"), "1\n")

    def test_workspace_marker_failure_restores_the_empty_desktop_ledger(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            destination = root / "desktop"
            _write_workspace_registry(source / "storages" / "workspace.json", 1)
            expected = _write_workspace_registry(destination / "storages" / "workspace.json", 0)

            with patch("server_manager._write_import_marker", side_effect=OSError("marker failure")):
                with self.assertRaisesRegex(OSError, "marker failure"):
                    _import_source_workspace(source, destination, root / "marker")

            self.assertEqual((destination / "storages" / "workspace.json").read_bytes(), expected)

    def test_completed_home_import_still_repairs_missing_workspace_grouping(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = ApplicationPaths.from_home(root / "app")
            source = root / "source"
            expected = _write_workspace_registry(source / "storages" / "workspace.json", 2)
            _write_workspace_registry(paths.dsh_home / "storages" / "workspace.json", 0)
            (paths.dsh_home / "settings.yaml").write_text("desktop: true\n", encoding="utf-8")
            paths.app_home.mkdir(parents=True, exist_ok=True)
            paths.home_import_marker.write_text("1\n", encoding="utf-8")

            _service_environment(paths, {}, source, root / "cc-switch")

            self.assertEqual((paths.dsh_home / "storages" / "workspace.json").read_bytes(), expected)
            self.assertEqual(paths.workspace_import_marker.read_text(encoding="utf-8"), "1\n")
            log = paths.server_log.read_text(encoding="utf-8")
            self.assertIn("source-home import v1 is already complete", log)
            self.assertIn("grouping=repaired", log)

    def test_default_service_home_runs_the_optional_cc_switch_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = ApplicationPaths.from_home(root / "app")
            source = root / "source"
            cc_switch_home = root / "cc-switch"

            with patch(
                "server_manager.import_cc_switch_configuration",
                return_value=CcSwitchImportResult(True, "completed: imported=2"),
            ) as importer:
                environment = _service_environment(paths, {}, source, cc_switch_home)

            importer.assert_called_once_with(
                cc_switch_home,
                paths.dsh_home,
                paths.cc_switch_import_marker,
            )
            self.assertEqual(environment["DSH_HOME"], str(paths.dsh_home))
            self.assertIn(
                "CC Switch import: completed: imported=2",
                paths.server_log.read_text(encoding="utf-8"),
            )

    def test_explicit_dsh_home_bypasses_the_desktop_import(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = ApplicationPaths.from_home(root / "app")
            source = root / "source"
            source.mkdir()
            (source / "settings.yaml").write_text("theme: source\n", encoding="utf-8")

            environment = _service_environment(
                paths,
                {"DSH_HOME": str(root / "chosen")},
                source,
                root / "cc-switch",
            )

            self.assertEqual(environment["DSH_HOME"], str(root / "chosen"))
            self.assertFalse(paths.dsh_home.exists())
            self.assertFalse(paths.workspace_import_marker.exists())
            self.assertIn("DSH_HOME is explicit", paths.server_log.read_text(encoding="utf-8"))

    def test_default_service_home_imports_source_configuration_once(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = ApplicationPaths.from_home(root / "app")
            source = root / "source"
            source.mkdir()
            (source / "settings.yaml").write_text("theme: source\n", encoding="utf-8")

            first = _service_environment(paths, {}, source, root / "cc-switch")
            (source / "settings.yaml").write_text("theme: changed\n", encoding="utf-8")
            second = _service_environment(paths, {}, source, root / "cc-switch")

            self.assertEqual(first["DSH_HOME"], str(paths.dsh_home))
            self.assertEqual(second["DSH_HOME"], str(paths.dsh_home))
            self.assertEqual(
                (paths.dsh_home / "settings.yaml").read_text(encoding="utf-8"),
                "theme: source\n",
            )
            self.assertEqual(paths.home_import_marker.read_text(encoding="utf-8"), "1\n")
            self.assertIn("already complete", paths.server_log.read_text(encoding="utf-8"))

    def test_existing_default_configuration_does_not_block_missing_history(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = ApplicationPaths.from_home(root / "app")
            source = root / "source"
            (source / "sessions").mkdir(parents=True)
            (source / "sessions" / "history").write_text("session\n", encoding="utf-8")
            (source / "attachments").mkdir()
            (source / "attachments" / "image").write_text("attachment\n", encoding="utf-8")
            (paths.dsh_home / "profiles" / "web").mkdir(parents=True)
            (paths.dsh_home / "settings.yaml").write_text("desktop: true\n", encoding="utf-8")

            _service_environment(paths, {}, source, root / "cc-switch")

            self.assertTrue((paths.dsh_home / "sessions" / "history").is_file())
            self.assertTrue((paths.dsh_home / "attachments" / "image").is_file())
            self.assertIn("history=copied", paths.server_log.read_text(encoding="utf-8"))

    def test_completed_import_does_not_sync_later_source_entries(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = ApplicationPaths.from_home(root / "app")
            source = root / "source"
            source.mkdir()
            (source / "settings.yaml").write_text("theme: source\n", encoding="utf-8")

            _service_environment(paths, {}, source, root / "cc-switch")
            (source / "AGENTS.md").write_text("later instructions\n", encoding="utf-8")
            _service_environment(paths, {}, source, root / "cc-switch")

            self.assertFalse((paths.dsh_home / "AGENTS.md").exists())

    def test_empty_configuration_reenables_a_completed_import(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = ApplicationPaths.from_home(root / "app")
            source = root / "source"
            source.mkdir()
            paths.app_home.mkdir(parents=True)
            paths.home_import_marker.write_text("1\n", encoding="utf-8")
            (source / "settings.yaml").write_text("theme: source\n", encoding="utf-8")

            _service_environment(paths, {}, source, root / "cc-switch")

            self.assertEqual(
                (paths.dsh_home / "settings.yaml").read_text(encoding="utf-8"),
                "theme: source\n",
            )


class TerminateTest(unittest.TestCase):
    def test_terminate_kills_child_process(self) -> None:
        proc = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(120)"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            **(_launch_options()),
        )
        try:
            self.assertIsNone(proc.poll())
            _terminate(proc.pid)
            proc.wait(timeout=10)
            self.assertIsNotNone(proc.poll())
        finally:
            if proc.poll() is None:
                _terminate(proc.pid, force=True)


if __name__ == "__main__":
    unittest.main()
