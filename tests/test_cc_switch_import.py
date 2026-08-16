# SPDX-License-Identifier: MIT
"""Tests for the isolated, read-only CC Switch provider import."""
from __future__ import annotations

import json
import os
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from cc_switch_import import (
    _publish_candidate,
    _publish_new_file,
    import_cc_switch_configuration,
)


def _create_database(home: Path, rows: list[dict[str, object]]) -> Path:
    home.mkdir(parents=True)
    database = home / "cc-switch.db"
    with sqlite3.connect(database) as connection:
        connection.execute(
            """CREATE TABLE providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                meta TEXT NOT NULL DEFAULT '{}',
                sort_index INTEGER,
                is_current BOOLEAN NOT NULL DEFAULT 0,
                PRIMARY KEY (id, app_type)
            )"""
        )
        for index, row in enumerate(rows):
            connection.execute(
                "INSERT INTO providers VALUES (?, ?, ?, ?, ?, ?, ?)",
                (
                    row["id"],
                    row.get("app_type", "claude"),
                    row["name"],
                    json.dumps(row["settings"]),
                    json.dumps(row.get("meta", {})),
                    index,
                    row.get("is_current", False),
                ),
            )
    return database


def _provider(
    provider_id: str,
    name: str,
    *,
    base_url: str,
    key: str,
    model: str,
    api_format: str = "anthropic",
    meta: dict[str, object] | None = None,
) -> dict[str, object]:
    metadata = {"apiFormat": api_format}
    metadata.update(meta or {})
    return {
        "id": provider_id,
        "name": name,
        "settings": {
            "env": {
                "ANTHROPIC_BASE_URL": base_url,
                "ANTHROPIC_AUTH_TOKEN": key,
                "ANTHROPIC_MODEL": model,
            },
        },
        "meta": metadata,
    }


class CcSwitchImportTest(unittest.TestCase):
    def test_imports_only_standalone_claude_providers_into_new_documents(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            dsh_home = root / "desktop" / "dsh-home"
            marker = root / "desktop" / ".cc-switch-import-v2"
            database = _create_database(
                cc_switch_home,
                [
                    _provider(
                        "deepseek-id",
                        "DeepSeek",
                        base_url="https://api.deepseek.com/anthropic",
                        key="fake-deepseek-key",
                        model="deepseek-v4-pro",
                    ),
                    _provider(
                        "kimi-id",
                        "Kimi",
                        base_url="https://api.moonshot.cn/v1",
                        key="fake-kimi-key",
                        model="kimi-k2.7-code",
                        api_format="openai_chat",
                    ),
                    _provider(
                        "oauth-id",
                        "Managed OAuth",
                        base_url="https://api.example.com/v1",
                        key="fake-oauth-token",
                        model="managed-model",
                        api_format="openai_responses",
                        meta={"providerType": "codex_oauth"},
                    ),
                    _provider(
                        "local-id",
                        "Local route",
                        base_url="http://127.0.0.1:15721/v1",
                        key="PROXY_MANAGED",
                        model="local-model",
                    ),
                    {
                        **_provider(
                            "codex-row",
                            "Wrong app",
                            base_url="https://api.example.com/v1",
                            key="fake-wrong-app-key",
                            model="wrong-model",
                        ),
                        "app_type": "codex",
                    },
                ],
            )
            original_database = database.read_bytes()
            original_mtime = database.stat().st_mtime_ns
            original_source_entries = {entry.name for entry in cc_switch_home.iterdir()}

            result = import_cc_switch_configuration(cc_switch_home, dsh_home, marker)

            self.assertTrue(result.imported)
            self.assertIn("imported=2", result.message)
            settings_text = (dsh_home / "settings.yaml").read_text(encoding="utf-8")
            credentials_text = (dsh_home / ".credentials.yaml").read_text(encoding="utf-8")
            settings = json.loads(settings_text)
            credentials = json.loads(credentials_text)
            routes = settings["llm-pi-ai"]["providers"]
            self.assertEqual(len(routes), 2)
            by_name = {route["displayName"]: route for route in routes.values()}
            self.assertEqual(by_name["DeepSeek (CC Switch)"]["api"], "anthropic-messages")
            self.assertEqual(by_name["Kimi (CC Switch)"]["api"], "openai-completions")
            self.assertEqual(
                by_name["DeepSeek (CC Switch)"]["baseURL"],
                "https://api.deepseek.com/anthropic",
            )
            self.assertEqual(
                by_name["Kimi (CC Switch)"]["models"],
                [{"id": "kimi-k2.7-code"}],
            )
            self.assertEqual(set(credentials.values()), {"fake-deepseek-key", "fake-kimi-key"})
            for secret in credentials.values():
                self.assertNotIn(secret, settings_text)
                self.assertNotIn(secret, result.message)
            self.assertEqual(marker.read_text(encoding="utf-8"), "1\n")
            self.assertEqual(database.read_bytes(), original_database)
            self.assertEqual(database.stat().st_mtime_ns, original_mtime)
            self.assertEqual(
                {entry.name for entry in cc_switch_home.iterdir()},
                original_source_entries,
            )
            if os.name != "nt":
                self.assertEqual((dsh_home / "settings.yaml").stat().st_mode & 0o777, 0o600)
                self.assertEqual((dsh_home / ".credentials.yaml").stat().st_mode & 0o777, 0o600)

    def test_existing_yaml_configuration_is_preserved_while_providers_are_added(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            dsh_home = root / "dsh-home"
            marker = root / "marker"
            _create_database(
                cc_switch_home,
                [
                    _provider(
                        "deepseek-id",
                        "DeepSeek",
                        base_url="https://api.deepseek.com/anthropic",
                        key="fake-key",
                        model="deepseek-v4-pro",
                    )
                ],
            )
            dsh_home.mkdir()
            expected = "locale:\n  preference: en\n"
            (dsh_home / "settings.yaml").write_text(expected, encoding="utf-8")

            result = import_cc_switch_configuration(cc_switch_home, dsh_home, marker)

            self.assertTrue(result.imported)
            merged = (dsh_home / "settings.yaml").read_text(encoding="utf-8")
            self.assertTrue(merged.startswith(expected))
            self.assertIn("llm-pi-ai:", merged)
            credentials = json.loads(
                (dsh_home / ".credentials.yaml").read_text(encoding="utf-8")
            )
            self.assertEqual(list(credentials.values()), ["fake-key"])
            self.assertEqual(marker.read_text(encoding="utf-8"), "1\n")

    def test_existing_yaml_credential_values_are_preserved_without_readback_logging(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            dsh_home = root / "dsh-home"
            marker = root / "marker"
            _create_database(
                cc_switch_home,
                [
                    _provider(
                        "deepseek-id",
                        "DeepSeek",
                        base_url="https://api.deepseek.com/anthropic",
                        key="fake-imported-key",
                        model="deepseek-v4-pro",
                    )
                ],
            )
            dsh_home.mkdir()
            original_settings = "ui-onboarding:\n  welcomeNoticeVersion: test\n"
            original_credentials = 'EXISTING_API_KEY: "fake-existing-key"\n'
            (dsh_home / "settings.yaml").write_text(original_settings, encoding="utf-8")
            (dsh_home / ".credentials.yaml").write_text(
                original_credentials,
                encoding="utf-8",
            )

            result = import_cc_switch_configuration(cc_switch_home, dsh_home, marker)

            self.assertTrue(result.imported)
            merged_settings = (dsh_home / "settings.yaml").read_text(encoding="utf-8")
            merged_credentials = (dsh_home / ".credentials.yaml").read_text(encoding="utf-8")
            self.assertTrue(merged_settings.startswith(original_settings))
            self.assertTrue(merged_credentials.startswith(original_credentials))
            self.assertIn("fake-imported-key", merged_credentials)
            self.assertNotIn("fake-existing-key", merged_settings)
            self.assertNotIn("fake-imported-key", merged_settings)
            self.assertNotIn("fake-existing-key", result.message)
            self.assertNotIn("fake-imported-key", result.message)

    def test_existing_unmergeable_provider_namespace_is_preserved_and_recorded(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            dsh_home = root / "dsh-home"
            marker = root / "marker"
            _create_database(
                cc_switch_home,
                [
                    _provider(
                        "deepseek-id",
                        "DeepSeek",
                        base_url="https://api.deepseek.com/anthropic",
                        key="fake-key",
                        model="deepseek-v4-pro",
                    )
                ],
            )
            dsh_home.mkdir()
            existing = "llm-pi-ai:\n  providers:\n    custom: {}\n"
            (dsh_home / "settings.yaml").write_text(existing, encoding="utf-8")

            result = import_cc_switch_configuration(cc_switch_home, dsh_home, marker)

            self.assertFalse(result.imported)
            self.assertIn("no safe additions", result.message)
            self.assertEqual((dsh_home / "settings.yaml").read_text(encoding="utf-8"), existing)
            self.assertFalse((dsh_home / ".credentials.yaml").exists())
            self.assertEqual(marker.read_text(encoding="utf-8"), "1\n")

    def test_absent_database_is_recorded_as_the_only_install_time_decision(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            dsh_home = root / "dsh-home"
            marker = root / "marker"

            first = import_cc_switch_configuration(cc_switch_home, dsh_home, marker)
            _create_database(
                cc_switch_home,
                [
                    _provider(
                        "later-id",
                        "Installed later",
                        base_url="https://api.example.com/anthropic",
                        key="fake-later-key",
                        model="later-model",
                    )
                ],
            )
            second = import_cc_switch_configuration(cc_switch_home, dsh_home, marker)

            self.assertFalse(first.imported)
            self.assertIn("without a CC Switch database", first.message)
            self.assertFalse(second.imported)
            self.assertIn("already complete", second.message)
            self.assertEqual(marker.read_text(encoding="utf-8"), "1\n")
            self.assertFalse((dsh_home / "settings.yaml").exists())

    def test_v1_marker_does_not_suppress_the_corrected_v2_merge(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            dsh_home = root / "desktop" / "dsh-home"
            v1_marker = root / "desktop" / ".cc-switch-import-v1"
            v2_marker = root / "desktop" / ".cc-switch-import-v2"
            _create_database(
                cc_switch_home,
                [
                    _provider(
                        "deepseek-id",
                        "DeepSeek",
                        base_url="https://api.deepseek.com/anthropic",
                        key="fake-key",
                        model="deepseek-v4-pro",
                    )
                ],
            )
            dsh_home.mkdir(parents=True)
            (dsh_home / "settings.yaml").write_text(
                "ui-onboarding:\n  welcomeNoticeVersion: test\n",
                encoding="utf-8",
            )
            v1_marker.write_text("1\n", encoding="utf-8")

            result = import_cc_switch_configuration(cc_switch_home, dsh_home, v2_marker)

            self.assertTrue(result.imported)
            self.assertEqual(v1_marker.read_text(encoding="utf-8"), "1\n")
            self.assertEqual(v2_marker.read_text(encoding="utf-8"), "1\n")
            self.assertIn(
                "llm-pi-ai:",
                (dsh_home / "settings.yaml").read_text(encoding="utf-8"),
            )

    def test_completed_import_never_syncs_later_cc_switch_changes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            dsh_home = root / "dsh-home"
            marker = root / "marker"
            _create_database(
                cc_switch_home,
                [
                    _provider(
                        "deepseek-id",
                        "DeepSeek",
                        base_url="https://api.deepseek.com/anthropic",
                        key="fake-original-key",
                        model="deepseek-v4-pro",
                    )
                ],
            )
            import_cc_switch_configuration(cc_switch_home, dsh_home, marker)
            expected = (dsh_home / ".credentials.yaml").read_bytes()
            with sqlite3.connect(cc_switch_home / "cc-switch.db") as connection:
                settings = {
                    "env": {
                        "ANTHROPIC_BASE_URL": "https://api.deepseek.com/anthropic",
                        "ANTHROPIC_AUTH_TOKEN": "fake-changed-key",
                        "ANTHROPIC_MODEL": "deepseek-v4-flash",
                    }
                }
                connection.execute(
                    "UPDATE providers SET settings_config = ? WHERE id = ?",
                    (json.dumps(settings), "deepseek-id"),
                )

            result = import_cc_switch_configuration(cc_switch_home, dsh_home, marker)

            self.assertFalse(result.imported)
            self.assertIn("already complete", result.message)
            self.assertEqual((dsh_home / ".credentials.yaml").read_bytes(), expected)

    def test_publication_failure_rolls_back_every_created_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            dsh_home = root / "dsh-home"
            marker = root / "marker"
            _create_database(
                cc_switch_home,
                [
                    _provider(
                        "deepseek-id",
                        "DeepSeek",
                        base_url="https://api.deepseek.com/anthropic",
                        key="fake-key",
                        model="deepseek-v4-pro",
                    )
                ],
            )
            calls = 0

            def fail_second(source: Path, target: Path, expected: bytes | None) -> None:
                nonlocal calls
                calls += 1
                if calls == 2:
                    raise OSError("simulated publication failure")
                _publish_candidate(source, target, expected)

            with patch("cc_switch_import._publish_candidate", side_effect=fail_second):
                result = import_cc_switch_configuration(cc_switch_home, dsh_home, marker)

            self.assertFalse(result.imported)
            self.assertIn("safe publication rollback", result.message)
            self.assertFalse((dsh_home / "settings.yaml").exists())
            self.assertFalse((dsh_home / ".credentials.yaml").exists())
            self.assertEqual(marker.read_text(encoding="utf-8"), "1\n")

    def test_merge_publication_failure_restores_exact_existing_documents(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            dsh_home = root / "dsh-home"
            marker = root / "marker"
            _create_database(
                cc_switch_home,
                [
                    _provider(
                        "deepseek-id",
                        "DeepSeek",
                        base_url="https://api.deepseek.com/anthropic",
                        key="fake-imported-key",
                        model="deepseek-v4-pro",
                    )
                ],
            )
            dsh_home.mkdir()
            original_settings = b"ui-onboarding:\n  welcomeNoticeVersion: test\n"
            original_credentials = b'EXISTING_API_KEY: "fake-existing-key"\n'
            (dsh_home / "settings.yaml").write_bytes(original_settings)
            (dsh_home / ".credentials.yaml").write_bytes(original_credentials)
            calls = 0

            def fail_second(source: Path, target: Path, expected: bytes | None) -> None:
                nonlocal calls
                calls += 1
                if calls == 2:
                    raise OSError("simulated merged publication failure")
                _publish_candidate(source, target, expected)

            with patch("cc_switch_import._publish_candidate", side_effect=fail_second):
                result = import_cc_switch_configuration(cc_switch_home, dsh_home, marker)

            self.assertFalse(result.imported)
            self.assertEqual((dsh_home / "settings.yaml").read_bytes(), original_settings)
            self.assertEqual(
                (dsh_home / ".credentials.yaml").read_bytes(),
                original_credentials,
            )
            self.assertEqual(marker.read_text(encoding="utf-8"), "1\n")

    def test_failed_file_copy_removes_its_exclusive_target(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = root / "published.yaml"

            with self.assertRaises(OSError):
                _publish_new_file(root / "missing-staged-file", target)

            self.assertFalse(target.exists())

    def test_malformed_database_is_an_optional_safe_skip(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cc_switch_home = root / "cc-switch"
            cc_switch_home.mkdir()
            (cc_switch_home / "cc-switch.db").write_text("not sqlite", encoding="utf-8")

            result = import_cc_switch_configuration(
                cc_switch_home,
                root / "dsh-home",
                root / "marker",
            )

            self.assertFalse(result.imported)
            self.assertIn("could not be read safely", result.message)
            self.assertFalse((root / "dsh-home" / "settings.yaml").exists())
            self.assertEqual((root / "marker").read_text(encoding="utf-8"), "1\n")


if __name__ == "__main__":
    unittest.main()
